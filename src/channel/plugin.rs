use std::{ffi::OsStr, path::Path, sync::Arc};

use anyhow::Error;
use async_trait::async_trait;
use channel_plugin::plugin_actor::{spawn_plugin, spawn_plugin_actor, PluginHandle};
use tracing::{error, info, warn};

use crate::watcher::{DirectoryWatcher, WatchedType};
//use once_cell::sync::OnceCell;

//use crate::{flow::session::SessionStore,};

/// reference SessionStore for plugins
//pub static SESSION_STORE: OnceCell<SessionStore> = OnceCell::new();


/// Called whenever a .so/.dll is added, changed, or removed.
#[async_trait]
pub trait PluginEventHandler: Send + Sync + 'static {
    /// A plugin named `name` has just been loaded or re-loaded.
    async fn plugin_added_or_reloaded(&self, name: &str, plugin: Arc<&PluginHandle>) -> Result<(),Error>;

    /// A plugin named `name` has just been removed.
    async fn plugin_removed(&self, name: &str)  -> Result<(),Error>;
}

/// Holds all currently‐loaded plugins and knows how to reload them.
pub struct PluginWatcher {
    dir: PathBuf,
    pub plugins: DashMap<String, PluginHandle>,
    subscribers: Mutex<Vec<Arc<dyn PluginEventHandler>>>,
    path_to_name: DashMap<String,String>,
}

impl PluginWatcher {
    pub fn new(dir: PathBuf) -> Self {
        // pre-load everything on startup
        let plugins = DashMap::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if let Some(ext) = p.extension().and_then(OsStr::to_str) {
                if ["exe",""].contains(&ext) {
                    if let Ok(plugin) = PluginHandle::spawn_plugin(p.clone()) as PluginHandle {
                        let name = plugin.id();
                        plugins.insert(name, Arc::new(plugin));
                    }
                }
            }
        }

        PluginWatcher {
            dir,
            plugins,
            subscribers: Mutex::new(Vec::new()),
            path_to_name: DashMap::new(),
        }
    }

    /// Start watching the plugin directory for add/modify/remove events.
    /// Returns a JoinHandle for the spawned watcher task.
    pub async fn watch(self: Arc<Self>) -> Result<DirectoryWatcher, Error> {
        // We know `PluginWatcher` already implements `WatchedType`
        let dir = self.dir.clone();
        let watcher: Arc<dyn WatchedType> = self.clone();
        DirectoryWatcher::new(dir, watcher, &["exe", "",], true).await
    }

    pub fn get(&self, name: &str) -> Option<Arc<PluginHandle>> {
        self.plugins
            .get(name)                     // returns Option<Ref<'_, String, Arc<Plugin>>>
            .map(|entry| entry.value().clone())
    }

    /// Subscribe for plugin add/reload/remove events.
    pub async fn subscribe(&self, handler: Arc<dyn PluginEventHandler>, notify: bool) {
        // First, register the handler
        {
            let mut subs = self.subscribers.lock().unwrap();
            subs.push(handler.clone());
        }

        if notify {
            // Then notify it of all existing plugins
            for entry in self.plugins.iter() {
                // entry.key()  -> &String
                // entry.value() -> &Arc<Plugin>
                let name = entry.key();                         // borrow key
                let plugin = Arc::new(entry.value());         // clone the Arc so you own one
                if let Err(err) = handler
                    .plugin_added_or_reloaded(name, plugin)
                    .await
                {
                    warn!("Could not load plugin {}: {:?}", name, err);
                }
            }
        }
    }

     /// Notify all subscribers that `name` was added or reloaded.
    async fn notify_add_or_reload(&self, name: &str, plugin: Arc<&PluginHandle>) {
        let subs = self.subscribers.lock().unwrap().clone();
        for sub in subs {
            let result = sub.plugin_added_or_reloaded(name, Arc::clone(&plugin)).await;
            if result.is_err() {
                warn!("Could not reload plugin {}",name);
            }
        }
    }

    /// Notify all subscribers that `name` was removed.
    async fn notify_removal(&self, name: &str) {
        let subs = self.subscribers.lock().unwrap().clone();
        for sub in subs {
            let result = sub.plugin_removed(name).await;
            if result.is_err() {
                warn!("Could not remove plugin {}",name);
            }
        }
    }


}


#[async_trait]
impl crate::watcher::WatchedType for PluginWatcher {
    fn is_relevant(&self, path: &Path) -> bool {
        path.parent().map(|d| d == self.dir).unwrap_or(false)
            && path.extension().and_then(OsStr::to_str).map_or(false, |e| {
                ["","exe",].contains(&e)
            })
    }

    async fn on_create_or_modify(&self, path: &Path) -> anyhow::Result<()> {
        let name = match Self::plugin_name(path) {
            Some(n) => n,
            None    => return Ok(()),
        };
        

        // load a brand new Plugin instance every time
        match spawn_plugin(path.to_path_buf()) {
            Ok(plugin) => {
                let plugin_name = plugin.id();
                self.plugins.insert(name, Arc::new(plugin));
                // now notify outside the lock
                let path_str = path.to_string_lossy().to_string();
                self.path_to_name.insert(path_str,plugin_name.clone());
                self.notify_add_or_reload(&plugin_name, Arc::new(&plugin)).await;
            }
            Err(err) => {
                error!("Could not load {} because {}", name, err);
            }
        }
        Ok(())
    }

    async fn on_remove(&self, path: &Path) -> anyhow::Result<()> {
        let path_str = path.to_string_lossy();
        if let Some(name_ref) = self.path_to_name.get(&path_str.to_string()) {
            // Synchronously remove under the lock, record whether we actually removed something:
            let plugin_name = name_ref.value().clone();
            self.plugins.remove(&plugin_name);
            info!("Unloading plugin `{}`", plugin_name);
            self.notify_removal(&plugin_name).await;
        }
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use crate::watcher::WatchedType;

    use super::*;
    use std::{
        collections::VecDeque, fs::{self, File}, path::PathBuf, sync::{Condvar, Mutex},
    };
    use channel_plugin::{message::{CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, InitParams, InitResult, ListKeysResult, MessageInResult, MessageOutParams, MessageOutResult, NameResult, StateResult, WaitUntilDrainedParams, WaitUntilDrainedResult}, plugin_runtime::{HasStore, PluginHandler}};
    use dashmap::DashMap;
    use tempfile::TempDir;
    //use tokio::sync::Notify;

    #[derive(Clone)]
    pub struct MockChannel {
        // queue for incoming
        messages: Arc<Mutex<VecDeque<ChannelMessage>>>,
        // condvar to wake up pollers
        cvar: Arc<Condvar>,
        outgoing: Arc<Mutex<Vec<ChannelMessage>>>,
        state: Arc<Mutex<ChannelState>>,
        config: DashMap<String, String>,
        secrets: DashMap<String, String>,
    }

    impl MockChannel {
        pub fn new() -> Arc<Self> {
                Arc::new(Self {
                    messages: Arc::new(Mutex::new(VecDeque::new())),
                    cvar: Arc::new(Condvar::new()),
                    outgoing: Arc::new(Mutex::new(vec![])),
                    state: Arc::new(Mutex::new(ChannelState::STARTING)),
                    config: DashMap::new(),
                    secrets: DashMap::new(),
                })
            }

        /// Inject an incoming message and wake any pollers.
        pub fn inject(&self, msg: ChannelMessage) {
            let mut q = self.messages.lock().unwrap();
            q.push_back(msg);
            // notify anyone blocked in `poll()`
            self.cvar.notify_one();
        }

        pub fn drain(&self) -> Vec<ChannelMessage> {
            let mut q = self.messages.lock().unwrap();
            q.drain(..).collect()
        }

        pub fn sent_messages(&self) -> Vec<ChannelMessage> {
            self.messages.lock().unwrap().iter().cloned().collect()
        }

    }

    impl HasStore for MockChannel {
        fn config_store(&self)  -> &DashMap<String, String> { &self.config }
        fn secret_store(&self)  -> &DashMap<String, String> { &self.secrets }
    }

    #[async_trait]
    impl PluginHandler for MockChannel {
        fn name(&self) -> NameResult {
            NameResult{name:"mock".into()}
        }


        fn capabilities(&self) -> CapabilitiesResult {
            CapabilitiesResult{capabilities:ChannelCapabilities {
                name: "mock".to_string(),
                supports_sending: true,
                supports_receiving: true,
                supports_text: true,
                supports_files: false,
                supports_media: false,
                supports_events: false,
                supports_typing: false,
                supports_threading: false,
                supports_routing: false,
                supports_reactions: false,
                supports_call: false,
                supports_buttons: false,
                supports_links: false,
                supports_custom_payloads: false,
                supported_events: vec![],
            }}
        }

        fn list_config_keys(&self) -> ListKeysResult {
            ListKeysResult { required_keys: vec![], optional_keys: vec![] }
        }

        fn list_secret_keys(&self) -> ListKeysResult {
            ListKeysResult { required_keys: vec![], optional_keys: vec![] }
        }

        async fn state(&self) -> StateResult {
            StateResult{state:*self.state.lock().unwrap()}
        }

        async fn init(&mut self, params: InitParams) -> InitResult {
            *self.state.lock().unwrap() = ChannelState::Running;
            Ok(InitResult{ success: true, error: None })
        }

        async fn drain(&mut self) {
            *self.state.lock().unwrap() = ChannelState::Draining;
        }

        async fn wait_until_drained(&self, _params: WaitUntilDrainedParams) -> WaitUntilDrainedResult {
            Ok(WaitUntilDrainedResult{ stopped: true, error: false })
        }

        async fn stop(&mut self) {
            *self.state.lock().unwrap() = ChannelState::Stopped;
        }
        
        async fn send_message(&mut self, params: MessageOutParams) -> MessageOutResult{

            self.outgoing.lock().unwrap().push(params.message);
            Ok(MessageOutResult{ success: true, error: None })
        }
        
        async fn receive_message(&mut self) -> MessageInResult{
            // grab the lock
            let mut guard = self.messages.lock().unwrap();
            // wait while empty
            while guard.is_empty() {
                guard = self.cvar.wait(guard).unwrap();
            }
            // at least one message—pop & return
            Ok(MessageInResult{message:guard.pop_front().unwrap()})
        }

    }

    #[test]
    fn plugin_name_extracts_stem() {
        let p = PathBuf::from("/foo/bar/baz.so");
        let name = PluginWatcher::plugin_name(&p);
        assert_eq!(name, Some("baz".into()));

        let p2 = PathBuf::from("/foo/bar/ignore.txt");
        let name2 = PluginWatcher::plugin_name(&p2);
        println!("{:?}",name2);
        assert_eq!(name2, None);
    }

    #[test]
    fn is_relevant_only_dylibs_in_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let watcher = PluginWatcher::new(dir.clone());

        let good = dir.join("plugin1.so");
        let bad_ext = dir.join("not_a_plugin.txt");
        let outside = PathBuf::from("/other/plugin2.so");

        assert!(watcher.is_relevant(&good));
        assert!(!watcher.is_relevant(&bad_ext));
        assert!(!watcher.is_relevant(&outside));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_skips_invalid_plugins() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        // create one valid‐looking file and one bogus
        let so = dir.join("a.so");
        File::create(&so).unwrap();
        let txt = dir.join("b.txt");
        File::create(&txt).unwrap();

        // Since `a.so` isn't a real library, Plugin::load will fail and skip it,
        // so watcher.plugins should be empty.
        let watcher = PluginWatcher::new(dir);
        assert!(watcher.plugins.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_create_or_modify_loads_new_plugin() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let watcher = PluginWatcher::new(dir.clone());

        // write an actual .so file path (still invalid but plugin_load errors are caught)
        let so = dir.join("new.so");
        File::create(&so).unwrap();

        // this should not panic
        watcher.on_create_or_modify(&so).await.unwrap();

        // since load failed, the map remains empty
        assert!(watcher.plugins.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_remove_unloads_plugin_safely() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let watcher = PluginWatcher::new(dir.clone());
        // Create the PathBuf we'll use for our dummy plugin:
        let so = dir.join("dummy.so");
        File::create(&so).unwrap();
        // Simulate a plugin in the map
        {
            // create a dummy Plugin with a real file path
            let fake = MockChannel::new();
            watcher.plugins.insert("dummy".into(), fake);
            watcher.path_to_name.insert(so.to_string_lossy().into_owned(), "dummy".to_string());
        }

        // remove a non-existent file – must not panic
        let bogus = dir.join("unknown.so");
        watcher.on_remove(&bogus).await.unwrap();

        // remove our dummy by path
        let p = dir.join("dummy.so");
        // trick plugin_name to extract "dummy"
        let _ = fs::File::create(&p); 
        watcher.on_remove(&p).await.unwrap();

        // map is now empty
        assert!(watcher.plugins.is_empty());
    }
}
