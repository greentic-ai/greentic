use std::{ffi::{c_char, CStr, CString, OsStr}, fs, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::SystemTime};
use anyhow::{Context, Error};
use async_trait::async_trait;
use channel_plugin::{message::{ChannelCapabilities, ChannelMessage}, plugin::{ChannelState, PluginLogger}, PluginHandle};
use dashmap::DashMap;
use libloading::{Library, Symbol};
use once_cell::sync::OnceCell;
use tracing::{error, info, warn};
use async_ffi::{BorrowingFfiFuture, FfiFuture};
use crate::{flow::session::SessionStore, watcher::{DirectoryWatcher, WatchedType}};

/// reference SessionStore for plugins
pub static SESSION_STORE: OnceCell<SessionStore> = OnceCell::new();
/// Callback functions for ChannelPlugins to get access to sessions
#[repr(C)]
#[derive(Copy, Clone)]
pub struct PluginSessionCallbacks {
    pub get_session: GetSessionFn,
    pub get_or_create_session: GetOrCreateSessionFn,
    pub invalidate_session: InvalidateSessionFn,
}


// function‐pointer types matching your `export_plugin!` C API:
type PluginCreate           = unsafe extern "C" fn() -> PluginHandle;
type PluginDestroy          = unsafe extern "C" fn(PluginHandle);
type PluginSetLogger        = unsafe extern "C" fn(PluginHandle, PluginLogger);
type PluginName             = unsafe extern "C" fn(PluginHandle) -> *mut c_char;
type PluginStart            = unsafe extern "C" fn(PluginHandle) -> FfiFuture<bool>;
type PluginDrain            = unsafe extern "C" fn(PluginHandle) -> bool;
type PluginStop             = unsafe extern "C" fn(PluginHandle) -> FfiFuture<bool>;
type PluginWaitUntilDrained = unsafe extern "C" fn(PluginHandle, u64) -> FfiFuture<bool>;
type PluginCaps             = unsafe extern "C" fn(PluginHandle, *mut ChannelCapabilities) -> bool;
type PluginState            = unsafe extern "C" fn(PluginHandle) -> ChannelState;
type PluginConfig           = unsafe extern "C" fn(PluginHandle, *const c_char);
type PluginSecrets          = unsafe extern "C" fn(PluginHandle, *const c_char);
type PluginList             = unsafe extern "C" fn(PluginHandle) -> *mut c_char;
type PluginFreeString       = unsafe extern "C" fn(*mut c_char);
type PluginSendMessage      = unsafe extern "C" fn(PluginHandle, *const ChannelMessage) -> FfiFuture<bool>;
type PluginReceiveMessage   = unsafe extern "C" fn(PluginHandle) -> FfiFuture<*mut c_char>;
type GetSessionFn           = extern "C" fn(*const c_char, *const c_char) -> FfiFuture<*mut c_char>;
type GetOrCreateSessionFn   = extern "C" fn(*const c_char, *const c_char) -> FfiFuture<*mut c_char>;
type InvalidateSessionFn    = extern "C" fn(*const c_char) -> FfiFuture<()>;
//type PluginAddRoute         = unsafe extern "C" fn(PluginHandle, *const c_char, *const c_char) -> bool;
//type PluginRemoveRoute      = unsafe extern "C" fn(PluginHandle, *const c_char) -> bool;
//type PluginListRoutes       = unsafe extern "C" fn(PluginHandle) -> *mut c_char;
type PluginSetSessionCallbacks = unsafe extern "C" fn(PluginHandle, PluginSessionCallbacks);
pub type RegisterSessionFns    = unsafe extern "C" fn(GetSessionFn,GetOrCreateSessionFn,InvalidateSessionFn,);
/// We keep the `Library` alive so that the symbol pointers remain valid.
#[derive(Debug)]
pub struct Plugin {
    pub lib: Option<Library>,
    pub handle: PluginHandle,

    // all the entry points we’ll call:
    pub destroy: PluginDestroy,
    pub set_logger:  PluginSetLogger,
    pub name:    PluginName, 
    pub start:   PluginStart,
    pub drain:   PluginDrain,
    pub stop:    PluginStop,
    pub wait_until_drained: PluginWaitUntilDrained,
    pub caps:               PluginCaps,
    pub state:              PluginState,
    pub set_secrets:        PluginSecrets,
    pub set_config:         PluginConfig,
    pub list_secrets:       PluginList,
    pub list_config:        PluginList,
    pub free_string:        PluginFreeString,
    pub send_message:       PluginSendMessage,
    pub receive_message:    PluginReceiveMessage,
   // pub add_route:          Option<PluginAddRoute>,
   // pub remove_route:       Option<PluginRemoveRoute>,
   // pub list_routes:        Option<PluginListRoutes>,
    /// track last modification so we can reload
    pub set_session_callbacks: Option<PluginSetSessionCallbacks>,
    pub last_modified: SystemTime,
    pub path: PathBuf,
}

impl Plugin {
/// load (or reload) from `path`
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let meta = fs::metadata(&path)
            .with_context(|| format!("stat `{}`", path.display()))?;
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        // 1) open the dynamic library
        let lib = unsafe { Library::new(&path) }
            .with_context(|| format!("loading `{}`", path.display()))?;

        // 2) grab each symbol, copy out its fn pointer so the Symbol<T> drops:
        let create_sym: Symbol<PluginCreate> =
            unsafe { lib.get(b"plugin_create") }.context("missing `plugin_create`")?;
        let create = *create_sym;

        let destroy_sym: Symbol<PluginDestroy> =
            unsafe { lib.get(b"plugin_destroy") }.context("missing `plugin_destroy`")?;
        let destroy = *destroy_sym;

        let set_logger_sym: Symbol<PluginSetLogger> =
            unsafe { lib.get(b"plugin_set_logger") }
                .context("missing `plugin_set_logger`")?;
        let set_logger = *set_logger_sym;

        let name_sym: Symbol<PluginName> =
            unsafe { lib.get(b"plugin_name") }.context("missing `plugin_name`")?;
        let name = *name_sym;

        let start_sym: Symbol<PluginStart> =
            unsafe { lib.get(b"plugin_start") }.context("missing `plugin_start`")?;
        let start = *start_sym;

        let drain_sym: Symbol<PluginDrain> =
            unsafe { lib.get(b"plugin_drain") }.context("missing `plugin_drain`")?;
        let drain = *drain_sym;

        let stop_sym: Symbol<PluginStop> =
            unsafe { lib.get(b"plugin_stop") }.context("missing `plugin_stop`")?;
        let stop = *stop_sym;

        let wait_sym: Symbol<PluginWaitUntilDrained> = unsafe {
            lib.get(b"plugin_wait_until_drained")
        }
        .context("missing `plugin_wait_until_drained`")?;
        let wait_until_drained = *wait_sym;

        let caps_sym: Symbol<PluginCaps> =
            unsafe { lib.get(b"plugin_capabilities") }.context("missing `plugin_capabilities`")?;
        let caps = *caps_sym;

        let state_sym: Symbol<PluginState> =
            unsafe { lib.get(b"plugin_state") }.context("missing `plugin_state`")?;
        let state = *state_sym;

        let cfg_sym: Symbol<PluginConfig> =
            unsafe { lib.get(b"plugin_set_config") }.context("missing `plugin_set_config`")?;
        let set_config = *cfg_sym;

        let sec_sym: Symbol<PluginSecrets> =
            unsafe { lib.get(b"plugin_set_secrets") }.context("missing `plugin_set_secrets`")?;
        let set_secrets = *sec_sym;

        let list_secrets_sym: Symbol<PluginList> =
            unsafe { lib.get(b"plugin_list_secrets") }.context("missing `plugin_list_secrets`")?;
        let list_secrets = *list_secrets_sym;

        let list_config_sym: Symbol<PluginList> =
            unsafe { lib.get(b"plugin_list_config") }.context("missing `plugin_list_config`")?;
        let list_config = *list_config_sym;

        let free_string_sym: Symbol<PluginFreeString> = 
            unsafe { lib.get(b"plugin_free_string") }.context("missing `plugin_free_string`")?;
        let free_string = *free_string_sym;

        // new async entrypoints
       let send_msg_sym: Symbol<PluginSendMessage> =
           unsafe { lib.get(b"plugin_send_message") }
           .context("missing `plugin_send_message`")?;
       let send_message = *send_msg_sym;

       let recv_msg_sym: Symbol<PluginReceiveMessage> =
           unsafe { lib.get(b"plugin_receive_message") }
           .context("missing `plugin_receive_message`")?;
       let receive_message = *recv_msg_sym;
/* 
       let add_route = unsafe {
            lib.get(b"plugin_add_route").ok().map(|sym: Symbol<PluginAddRoute>| *sym)
        };

        let remove_route = unsafe {
            lib.get(b"plugin_remove_route").ok().map(|sym: Symbol<PluginRemoveRoute>| *sym)
        };

        let list_routes = unsafe {
            lib.get(b"plugin_list_routes").ok().map(|sym: Symbol<PluginListRoutes>| *sym)
        };
        */

        let set_sessions_sym = unsafe { lib.get(b"plugin_set_session_callbacks") };

        let set_session_callbacks = match set_sessions_sym {
            Ok(sym) => Some(*sym),
            Err(_) => None, // Optional for backwards compatibility
        };

        // 3) actually construct the plugin instance
        let handle = unsafe { create() };

        Ok(Plugin {
            lib: Some(lib),
            handle,
            destroy,
            set_logger,
            name,
            start,
            drain,
            stop,
            wait_until_drained,
            caps,
            state,
            set_secrets,
            set_config,
            list_secrets,
            list_config,
            free_string,
            send_message,
            receive_message,
          //  add_route,
          //  remove_route,
          //  list_routes,
            set_session_callbacks,
            last_modified: mtime,
            path,
        })
    }

}

pub extern "C" fn plugin_get_session(plugin_name: *const c_char, key: *const c_char) -> FfiFuture<*mut c_char> {
        let plugin = unsafe { CStr::from_ptr(plugin_name).to_string_lossy().to_string() };
        let key = unsafe { CStr::from_ptr(key).to_string_lossy().to_string() };
        let store = SESSION_STORE.get().cloned().expect("SESSION_STORE not initialized correctly");
        BorrowingFfiFuture::new(async move {
            if let Some(session_id) = store.get_channel(&plugin, &key).await {
                CString::new(session_id).unwrap().into_raw()
            } else {
                std::ptr::null_mut()
            }
        })
    }


pub extern "C" fn plugin_get_or_create_session(plugin_name: *const c_char, key: *const c_char) -> FfiFuture<*mut c_char> {
    let plugin = unsafe { CStr::from_ptr(plugin_name).to_string_lossy().to_string() };
    let key = unsafe { CStr::from_ptr(key).to_string_lossy().to_string() };
    let store = SESSION_STORE.get().cloned().expect("SESSION_STORE not initialized correctly");
    BorrowingFfiFuture::new(async move {
        let session_id = store.get_or_create_channel(&plugin, &key).await;
        CString::new(session_id).unwrap().into_raw()
    })
}


pub extern "C" fn plugin_invalidate_session(session_id: *const c_char) -> BorrowingFfiFuture<'static, ()> {
    let session_id = unsafe { CStr::from_ptr(session_id).to_string_lossy().to_string() };
    let store = SESSION_STORE.get().cloned().expect("SESSION_STORE not initialized correctly");
    BorrowingFfiFuture::new(async move {
        store.remove(&session_id).await;
    })
}

unsafe impl Send for Plugin {}
unsafe impl Sync for Plugin {}


/// Called whenever a .so/.dll is added, changed, or removed.
#[async_trait]
pub trait PluginEventHandler: Send + Sync + 'static {
    /// A plugin named `name` has just been loaded or re-loaded.
    async fn plugin_added_or_reloaded(&self, name: &str, plugin: Arc<Plugin>) -> Result<(),Error>;

    /// A plugin named `name` has just been removed.
    async fn plugin_removed(&self, name: &str)  -> Result<(),Error>;
}

/// Holds all currently‐loaded plugins and knows how to reload them.
pub struct PluginWatcher {
    dir: PathBuf,
    pub plugins: DashMap<String, Arc<Plugin>>,
    subscribers: Mutex<Vec<Arc<dyn PluginEventHandler>>>,
    path_to_name: DashMap<String,String>,
}

impl PluginWatcher {
    pub fn new(dir: PathBuf) -> Self {
        // pre-load everything on startup
        let map = DashMap::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if let Some(ext) = p.extension().and_then(OsStr::to_str) {
                if ["so","dylib","dll"].contains(&ext) {
                    if let Ok(plugin) = Plugin::load(p.clone()) {
                        let name = get_name(&plugin);
                        map.insert(name, Arc::new(plugin));
                    }
                }
            }
        }

        PluginWatcher {
            dir,
            plugins: map,
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
        DirectoryWatcher::new(dir, watcher, &["so", "dll", "dylib"], true).await
    }

    pub fn get(&self, name: &str) -> Option<Arc<Plugin>> {
        self.plugins
            .get(name)                     // returns Option<Ref<'_, String, Arc<Plugin>>>
            .map(|entry| entry.value().clone())
    }

    /// Subscribe for plugin add/reload/remove events.
    pub async fn subscribe(&self, handler: Arc<dyn PluginEventHandler>) {
        // First, register the handler
        {
            let mut subs = self.subscribers.lock().unwrap();
            subs.push(handler.clone());
        }

        // Then notify it of all existing plugins
        for entry in self.plugins.iter() {
            // entry.key()  -> &String
            // entry.value() -> &Arc<Plugin>
            let name = entry.key();                         // borrow key
            let plugin = Arc::clone(entry.value());         // clone the Arc so you own one
            if let Err(err) = handler
                .plugin_added_or_reloaded(name, plugin)
                .await
            {
                warn!("Could not load plugin {}: {:?}", name, err);
            }
        }
    }

     /// Notify all subscribers that `name` was added or reloaded.
    async fn notify_add_or_reload(&self, name: &str, plugin: Arc<Plugin>) {
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

    pub fn plugin_name(path: &Path) -> Option<String> {
        match path.file_stem().and_then(OsStr::to_str).map(|s| s.to_string())
        {
            Some(name) => if name == "ignore" { None } else { Some(name) },
            None => None,

        }
    }

}

/// Get the name from the plugin
fn get_name(plugin: &Plugin) -> String {
    // 1) Call the `plugin_name` function pointer (unsafe FFI)
    let raw_name_ptr: *mut c_char = unsafe {
        // (plugin_arc.name) is the extern "C" fn(PluginHandle) -> *mut c_char
        (plugin.name)(plugin.handle)
    };

    // 2) Safely convert the C string into Rust String
    let plugin_name: String = unsafe {
        assert!(!raw_name_ptr.is_null(), "plugin_name returned NULL");
        let cstr = CStr::from_ptr(raw_name_ptr);
        let s = cstr.to_string_lossy().into_owned();
        // 3) free the C‐allocated string
        (plugin.free_string)(raw_name_ptr);
        s
    };
    plugin_name
}

#[async_trait]
impl crate::watcher::WatchedType for PluginWatcher {
    fn is_relevant(&self, path: &Path) -> bool {
        path.parent().map(|d| d == self.dir).unwrap_or(false)
            && path.extension().and_then(OsStr::to_str).map_or(false, |e| {
                ["so","dylib","dll"].contains(&e)
            })
    }

    async fn on_create_or_modify(&self, path: &Path) -> anyhow::Result<()> {
        let name = match Self::plugin_name(path) {
            Some(n) => n,
            None    => return Ok(()),
        };
        

        // load a brand new Plugin instance every time
        match Plugin::load(path.to_path_buf()) {
            Ok(plugin) => {
                let plugin_name = get_name(&plugin);
                let plugin_arc = Arc::new(plugin);
                {
                    self.plugins.insert(plugin_name.clone(), plugin_arc.clone());
                }
                // now notify outside the lock
                let path_str = path.to_string_lossy().to_string();
                self.path_to_name.insert(path_str,plugin_name.clone());
                self.notify_add_or_reload(&plugin_name, plugin_arc).await;
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
    use crate::{channel::manager::tests::make_noop_plugin, watcher::WatchedType};

    use super::*;
    use std::{
        collections::VecDeque, fs::{self, File}, path::PathBuf, sync::Condvar,
    };
    use channel_plugin::plugin::{ChannelPlugin, PluginError};
    use tempfile::TempDir;
    //use tokio::sync::Notify;

    pub struct MockChannel {
        // queue for incoming
        messages: Arc<Mutex<VecDeque<ChannelMessage>>>,
        // condvar to wake up pollers
        cvar: Arc<Condvar>,
        outgoing: Arc<Mutex<Vec<ChannelMessage>>>,
        state: Arc<Mutex<ChannelState>>,
        config: DashMap<String, String>,
        secrets: DashMap<String, String>,
        //notify: Arc<Notify>,
        logger: Option<PluginLogger>,
    }

    impl MockChannel {
        pub fn new() -> Self {
                Self {
                    messages: Arc::new(Mutex::new(VecDeque::new())),
                    cvar: Arc::new(Condvar::new()),
                    outgoing: Arc::new(Mutex::new(vec![])),
                    state: Arc::new(Mutex::new(ChannelState::Starting)),
                    config: DashMap::new(),
                    secrets: DashMap::new(),
                    //notify: Arc::new(Notify::new()),
                    logger: None,
                }
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

    #[async_trait]
    impl ChannelPlugin for MockChannel {
        fn name(&self) -> String {
            "mock".into()
        }
        fn set_logger(&mut self, logger: PluginLogger) {
            self.logger = Some(logger);
        }

        fn get_logger(&self) -> Option<PluginLogger> {
            self.logger
        }

        fn capabilities(&self) -> ChannelCapabilities {
            ChannelCapabilities {
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
            }
        }

        fn set_config(&mut self, config: DashMap<String, String>) {
            self.config = config;
        }

        fn list_config(&self) -> Vec<String> {
            vec![]
        }

        fn set_secrets(&mut self, secrets: DashMap<String, String>) {
            self.secrets = secrets;
        }

        fn list_secrets(&self) -> Vec<String> {
            vec![]
        }

        fn state(&self) -> ChannelState {
            *self.state.lock().unwrap()
        }

        async fn start(&mut self) -> Result<(), PluginError> {
            *self.state.lock().unwrap() = ChannelState::Running;
            Ok(())
        }

        fn drain(&mut self) -> Result<(), PluginError> {
            *self.state.lock().unwrap() = ChannelState::Draining;
            Ok(())
        }

        async fn wait_until_drained(&mut self, _timeout_ms: u64) -> Result<(), PluginError> {
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), PluginError> {
            *self.state.lock().unwrap() = ChannelState::Stopped;
            Ok(())
        }
        
        async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(),PluginError>{
            self.outgoing.lock().unwrap().push(msg);
            Ok(())
        }
        
        async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage,PluginError>{
            // grab the lock
            let mut guard = self.messages.lock().unwrap();
            // wait while empty
            while guard.is_empty() {
                guard = self.cvar.wait(guard).unwrap();
            }
            // at least one message—pop & return
            Ok(guard.pop_front().unwrap())
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
            let fake = make_noop_plugin();
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
