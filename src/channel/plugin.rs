use std::{ffi::OsStr, path::{Path, PathBuf}, sync::{Arc, Mutex}};

use anyhow::Error;
use async_trait::async_trait;
use channel_plugin::plugin_actor::PluginHandle;
use dashmap::DashMap;
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
    async fn plugin_added_or_reloaded(&self, name: &str, plugin: PluginHandle) -> Result<(),Error>;

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
    pub async fn new(dir: PathBuf) -> Self {
        // pre-load everything on startup
        let plugins: DashMap<String,PluginHandle> = DashMap::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension()
                .and_then(OsStr::to_str)
                .map(|ext| ["exe", "sh", ""].contains(&ext))
                .unwrap_or(true) // Accept files with no extension
            {
                if let Ok((plugin, _events)) = PluginHandle::from_exe_with_events(p.clone()).await {
                    let name = plugin.id();
                    plugins.insert(name.to_string(), plugin.clone());
                    info!("Loaded plugin: {}",name.to_string());
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

    pub fn get(&self, name: &str) -> Option<PluginHandle> {
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
                let plugin = entry.value();         // clone the Arc so you own one
                if let Err(err) = handler
                    .plugin_added_or_reloaded(name, plugin.clone())
                    .await
                {
                    warn!("Could not load plugin {}: {:?}", name, err);
                }
            }
        }
    }

     /// Notify all subscribers that `name` was added or reloaded.
    async fn notify_add_or_reload(&self, name: &str, plugin: &PluginHandle) {
        let subs = self.subscribers.lock().unwrap().clone();
        for sub in subs {
            let result = sub.plugin_added_or_reloaded(name,plugin.clone()).await;
            if result.is_err() {
                warn!("Could not reload plugin {}",name);
            } else {
                info!("Loaded plugin: {}",name.to_string());
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
            }else {
                info!("Removed plugin: {}",name.to_string());
            }
        }
    }


}


#[async_trait]
impl crate::watcher::WatchedType for PluginWatcher {
    fn is_relevant(&self, path: &Path) -> bool {
        path.parent().map(|d| d == self.dir).unwrap_or(false)
            && match path.extension().and_then(OsStr::to_str) {
                Some(ext) => ["exe", "sh"].contains(&ext),
                None => true, // Accept files with no extension
            }
    }

    async fn on_create_or_modify(&self, path: &Path) -> anyhow::Result<()> {
        match PluginHandle::from_exe_with_events(path).await {
            Ok((plugin, _events)) => {
                let name = plugin.id();
                self.plugins.insert(name.to_string(), plugin.clone());
                let path_str = path.to_string_lossy().to_string();
                self.path_to_name.insert(path_str,name.to_string());
                self.notify_add_or_reload(&name, &plugin).await;
            } 
            Err(err) => {
                error!("Could not load {:?} because {:?}",path, err);
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
    use crate::{watcher::WatchedType};

    use super::*;
    use std::{
        fs::{self, File}, path::PathBuf,
    };
    use channel_plugin::plugin_test_util::{make_mock_handle, MockChannel};
    use tempfile::TempDir;


    #[tokio::test]
    async fn test_mock_channel() {
        let handle = make_mock_handle().await;

        let caps = handle.capabilities().await.unwrap();
        assert_eq!(caps.name, "mock");
    }


    #[tokio::test(flavor = "current_thread")]
    async fn is_relevant_only_dylibs_in_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let watcher = PluginWatcher::new(dir.clone()).await;

        let good = dir.join("plugin1");
        let bad_ext = dir.join("not_a_plugin.txt");
        let outside = PathBuf::from("/other/plugin2.dll");

        assert!(watcher.is_relevant(&good));
        assert!(!watcher.is_relevant(&bad_ext));
        assert!(!watcher.is_relevant(&outside));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_skips_invalid_plugins() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        // create one valid‐looking file and one bogus
        let so = dir.join("a.dll");
        File::create(&so).unwrap();
        let txt = dir.join("b.txt");
        File::create(&txt).unwrap();

        // Since `a.so` isn't a real library, Plugin::load will fail and skip it,
        // so watcher.plugins should be empty.
        let watcher = PluginWatcher::new(dir).await;
        assert!(watcher.plugins.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_create_or_modify_loads_new_plugin() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let watcher = PluginWatcher::new(dir.clone()).await;

        // write an actual .so file path (still invalid but plugin_load errors are caught)
        let exe = dir.join("new.exe");
        File::create(&exe).unwrap();

        // this should not panic
        watcher.on_create_or_modify(&exe).await.unwrap();

        // since load failed, the map remains empty
        assert!(watcher.plugins.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_remove_unloads_plugin_safely() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let watcher = PluginWatcher::new(dir.clone()).await;
        // Create the PathBuf we'll use for our dummy plugin:
        let so = dir.join("dummy.exe");
        File::create(&so).unwrap();
        // Simulate a plugin in the map
        {
            // create a dummy Plugin with a real file path
            let plugin_handle = Arc::new(MockChannel::new()).get_plugin_handle().await;
            watcher.plugins.insert("dummy".into(), plugin_handle);
            watcher.path_to_name.insert(so.to_string_lossy().into_owned(), "dummy".to_string());
        }

        // remove a non-existent file – must not panic
        let bogus = dir.join("unknown.exe");
        watcher.on_remove(&bogus).await.unwrap();

        // remove our dummy by path
        let p = dir.join("dummy.exe");
        // trick plugin_name to extract "dummy"
        let _ = fs::File::create(&p); 
        watcher.on_remove(&p).await.unwrap();

        // map is now empty
        assert!(watcher.plugins.is_empty());
    }
}
