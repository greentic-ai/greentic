// src/channel/manager.rs

use std::{
 fmt, path::PathBuf, sync::{Arc, Mutex},
};
use anyhow::{Error, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use futures::stream::{self, StreamExt};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use tokio_util::sync::CancellationToken;
use channel_plugin::{message::{ChannelMessage, ChannelState}, plugin_actor::PluginHandle, plugin_helpers::PluginError};

use crate::{
    channel::{plugin::{PluginEventHandler, PluginWatcher}, wrapper::PluginWrapper}, config::ConfigManager, flow::session::SessionStore, logger::LogConfig, secret::SecretsManager, watcher::DirectoryWatcher
};


/// in src/channel/manager.rs

/// a subscriber gets every incoming ChannelMessage
#[async_trait]
pub trait IncomingHandler: Send + Sync {
    async fn handle_incoming(&self, msg: ChannelMessage, session_store: SessionStore);
}


/// Manages all dynamically‐loaded "channels" (.so/.dll) and routes their messages.
#[derive(Clone)]
pub struct ChannelManager {
    config:        ConfigManager,
    secrets:       SecretsManager,
    store:         SessionStore,
    channels:      Arc<DashMap<String, ManagedChannel>>,
    log_config:    LogConfig,
    incoming_subscribers: Arc<Mutex<Vec<Arc<dyn IncomingHandler>>>>,
}

impl ChannelManager {
    /// Create a new manager that watches `plugins_dir` for channel plugins.
    pub async fn new(
        config: ConfigManager,
        secrets: SecretsManager,
        store: SessionStore,
        log_config: LogConfig,
    ) -> Result<Arc<Self>> {
        let me = Arc::new(Self {
            config,
            secrets,
            store,
            channels: Arc::new(DashMap::new()),
            log_config,
            incoming_subscribers: Arc::new(Mutex::new(vec![])),
        });

        // **3)** return your manager — it's already got the subscription in place.
        Ok(me)
    }

    /// Simple diagnostics: channel name → state
    pub async fn diagnostics(&self) -> DashMap<String, ChannelState> {
        let results = stream::iter(self.channels.iter())
            .then(|kv| {
                let key = kv.key().clone();
                let wrapper = kv.value().wrapper.clone();
                async move {
                    let state = wrapper.state().await;
                    (key, state)
                }
            })
            .collect::<Vec<_>>()
            .await;

        results.into_iter().collect()
    }

    pub fn session_store(&self) -> SessionStore {
        self.store.clone()
    }

    /// subscribe to incoming messageds
    pub fn subscribe_incoming(&self, h: Arc<dyn IncomingHandler>) {
        self.incoming_subscribers.lock().unwrap().push(h);
    }

    /// Load & start a channel plugin immediately.
    pub async fn register_channel(&self, name: String, wrapper: ManagedChannel) -> Result<(), PluginError> {
        self.channels.insert(name, wrapper);
        Ok(())
    }

    /// Unload & stop a channel by name.
    pub async fn unload_channel(&self, name: &str) -> Result<(), PluginError> {
        if let Some((_, mut wrapper)) = self.channels.remove(name) {
            wrapper.wrapper.stop().await?;
        }
        Ok(())
    }

    
    /// Start (or restart) a currently‐loaded channel.
    pub async fn start_channel(&self, name: &str) -> Result<(), PluginError> {
        if let Some(mut entry) = self.channels.get_mut(name) {
            if entry.value_mut().wrapper.state().await == ChannelState::RUNNING {
                info!("Ignoring start_channel {} because it is already staretd.",name);
                return Ok(()); // Already started
            }
            let config = self.config.0.as_ref().as_vec().await;
            let secrets = self.secrets.0.as_ref().as_vec().await;
            entry.value_mut().wrapper.start(config, secrets).await?;
        }
        Ok(())
    }
    

    /// Get a channel.
    pub fn channel(&self, name: &str) -> Option<PluginWrapper> {
        if let Some(entry) = self.channels.get(name) {
            Some(entry.value().wrapper.clone())
        } else {
            None
        }

    }

    /// Stop (but keep loaded) a channel.
    pub async fn stop_channel(&self, name: &str) -> Result<(), PluginError> {
        if let Some(mut entry) = self.channels.get_mut(name) {
            entry.value_mut().wrapper.stop().await?;
        }
        Ok(())
    }

    /// Send a message into a running plugin.  
    /// Returns Err if the plugin isn't loaded or send() fails.
    pub async fn send_to_channel(
        &self,
        name: &str,
        msg: ChannelMessage,
    ) -> Result<(), PluginError> {
        if let Some(mut wrapper) = self.channels.get_mut(name) {
            wrapper.wrapper.send_message(msg).await
        } else {
            Err(PluginError::Other(format!("channel `{}` not loaded", name)))
        }
    }

    /// List the names of all loaded channels.
    pub fn list_channels(&self) -> Vec<String> {
        self.channels.iter().map(|kv| kv.key().clone()).collect()
    }

    /// Get all loaded channels.
    pub fn channels(&self) -> Arc<DashMap<String,ManagedChannel>> {
        self.channels.clone()
    }

    /// Start watching the plugins directory, subscribe ourselves,
    /// and spawn the watcher task.
    ///
    /// Returns a JoinHandle so you can abort on shutdown.
    pub async fn start_all(self: Arc<Self>, plugins_dir: PathBuf) -> Result<DirectoryWatcher, Error> {
        // 1) build the watcher
        let watcher = Arc::new(PluginWatcher::new(plugins_dir.clone()).await);
        // 2) subscribe us to get add/remove events
        watcher.subscribe(self.clone() as Arc<dyn PluginEventHandler>, false).await;
        // 3) spawn the fs watcher
        match watcher.watch().await{
            Ok(handle) => Ok(handle),
            Err(err) => {
                let error = format!("Could not watch the channel plugsin at {}",plugins_dir.to_string_lossy());
                error!(error);
                Err(err)
            },
        }
    }

    /// Gracefully (or force‐) shut down every channel.
    pub fn shutdown_all(&self, graceful: bool, timeout_ms: u64) {
        // Kick off drain/stop on each one.
        for kv in self.channels.iter() {
            let mut w = kv.value().wrapper.clone();
            if graceful {
                let _ = w.drain();
            } else {
                let _ = w.stop();
            }
        }
        // If draining, wait them out.
        if graceful {
            for kv in self.channels.iter() {
                let mut w = kv.value().wrapper.clone();
                let _ = w.wait_until_drained(timeout_ms);
            }
        }
    }
}

#[async_trait]
impl PluginEventHandler for ChannelManager {
    /// Called when a `.so`/`.dll` is added or changed.
    async fn plugin_added_or_reloaded(&self, name: &str, plugin: PluginHandle) -> Result<(), Error> {
        info!("Channel plugin added/reloaded: {}", name);
        // If already present, tear down the old one:
        if let Some(mut old_plugin) = self.channels.get_mut(name) {
            // stopping the channel
            // signal its poller to exit
            // signal its poller to exit
            let mut wrapper = old_plugin.wrapper().clone();
            if wrapper.stop().await.is_err() {
                info!("Could not stop the existing plugin {} ",name);
            }
            old_plugin.cancel.as_ref().map(|tok| tok.cancel());
            // wait for it to actually stop
            old_plugin.poller.as_ref().map(|poller| poller.abort());
            if let Err(e) = old_plugin.wrapper.stop().await {
                info!("Could not stop existing plugin `{}`: {:?}", name, e);
            }
            // Now remove it from the map
            // (we must drop the `RefMut` before calling `remove`)
            drop(old_plugin);
            self.channels.remove(name);
            info!("— replaced existing channel `{}`", name);
        }

        // Wrap + configure:

        let wrapper = PluginWrapper::new(plugin, self.store.clone(), self.log_config.clone()).await;
           
        // 3) Start it **on its own thread** with its own runtime
        let mut wrapper_cloned = wrapper.clone();
        let plugin_name = name.to_string();

        // Add the session callbacks so the channel can get sessions
        //wrapper.set_session_callbacks();

        // run start() under that runtime
        let config = self.config.0.as_vec().await;
        let secrets = self.secrets.0.as_vec().await;
        match wrapper_cloned.start(config, secrets).await {
            Ok(()) => tracing::info!("Plugin `{}` started", plugin_name),
            Err(e) => tracing::error!("Failed to start `{}`: {:?}", plugin_name, e),
        }

        
        // 5) Spawn its polling loop
        let caps = wrapper.capabilities().await;
        if caps.supports_receiving {
            let channel_name = name.to_string();
            let subs = self.incoming_subscribers.clone();

            // create your cancellation token *once*
            let cancel_token = CancellationToken::new();
            // clone exactly for the poller
            let poller_cancel = cancel_token.clone();
            let poller_wrapper = wrapper.clone();

            let store = self.store.clone();
            let poller = tokio::spawn(async move {
                // run this entire loop inside a single blocking thread:
                // that way there's exactly one `.receive_message()` in flight at a time.
                loop {
                    let store = store.clone();
                    // check for cancellation _before_ we block again
                    if poller_cancel.is_cancelled() {
                         break;
                    }

                    let mut w = poller_wrapper.clone();
                    let poll_result = w.receive_message().await;
                    match poll_result {
                        Ok(mut msg) => {
                            // got a real message
                            msg.channel = channel_name.clone();

                            // snapshot & drop the lock quickly
                            let handlers = {
                                let guard = subs.lock().unwrap();
                                guard.clone()
                            };

                            // now dispatch in async land
                            for h in handlers {
                                let m = msg.clone();
                                let store = store.clone();
                                tokio::spawn(async move {
                                    let _ = h.handle_incoming(m, store.clone()).await;
                                });
                            }
                        }
                        Err(err) => {
                            tracing::warn!(%channel_name, ?err, "plugin.receive_message() returned error");
                            // you might want a small backoff here to avoid a busy loop
                        }
                    }
                }
            });

            // now you still have the original `wrapper`, and you can move it into your map
            self.channels.insert(name.to_string(), ManagedChannel {
                wrapper,
                cancel: Some(cancel_token),
                poller: Some(poller),
            });

        } else {
             self.channels.insert(name.to_string(), ManagedChannel {
                wrapper,
                cancel:None,
                poller:None, 
            });           
        }

        Ok(())
    }

    /// Called when a `.so`/`.dll` is removed.
    async fn plugin_removed(&self, name: &str) -> Result<(), Error> {
        if let Some(mut old_plugin) = self.channels.get_mut(name) {
            // stopping the channel
            let mut wrapper = old_plugin.wrapper().clone();
            let drain_result = wrapper.drain().await;
            if drain_result.is_err() {
                info!("Could not start draining the existing plugin {} ",name);
            }
            // try for 3 seconds to drain
            let is_drained_result = wrapper.wait_until_drained(3000).await;
            if is_drained_result.is_err() {
                info!("Could not drain the existing plugin {} ",name);
            }
            // signal its poller to exit
            old_plugin.cancel().as_ref().map(|tok| tok.cancel());
            
            // wait for it to actually stop
            old_plugin.poller().as_ref().map(|poller| poller.abort());
            if let Err(e) = old_plugin.wrapper.stop().await {
                info!("Could not stop existing plugin `{}`: {:?}", name, e);
            }
            // Now remove it from the map
            // (we must drop the `RefMut` before calling `remove`)
            drop(old_plugin);
            self.channels.remove(name);
            info!("— replaced existing channel `{}`", name);
        } else {
            warn!("Tried to remove unknown channel plugin: {}", name);
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct ManagedChannel {
    wrapper: PluginWrapper,
    cancel:  Option<CancellationToken>,
    poller:  Option<JoinHandle<()>>,
}

impl ManagedChannel {
    pub fn new(wrapper: PluginWrapper, cancel: Option<CancellationToken>, poller:  Option<JoinHandle<()>>) -> Self {
        Self {wrapper,cancel, poller}
    }

    fn cancel(&self) -> &Option<CancellationToken> {
        &self.cancel
    }

    fn poller(&mut self) -> &Option<JoinHandle<()>> {
        &self.poller
    }

    pub fn wrapper(&self) -> &PluginWrapper {
        &self.wrapper
    }
}

impl fmt::Debug for ChannelManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Snapshot how many channels are registered
        let channel_names: Vec<String> = self
            .channels
            .iter()
            .map(|kv| kv.key().clone())
            .collect();

        // Peek at how many incoming_subscribers there are (we lock the mutex briefly)
        let subscriber_count = {
            // We deliberately do a non-blocking lock here to avoid deadlocks in Debug,
            // but you could also use `block_in_place` or unwrap if you know it’s uncontested.
            match self.incoming_subscribers.try_lock() {
                Ok(vec) => vec.len(),
                Err(_) => usize::MAX, // or “<locked>” if you prefer
            }
        };

        f.debug_struct("ChannelManager")
            .field("config", &self.config)
            .field("secrets", &self.secrets)
            .field("channel_names", &channel_names)
            .field("host_logger", &"<HostLogger - no Debug>") 
            .field("incoming_subscriber_count", &subscriber_count)
            .finish()
    }
}


//
// Tests (bring ChannelPlugin into scope for PluginWrapper methods)
//

#[cfg(test)]
pub mod tests {
    use channel_plugin::plugin_test_util::{spawn_mock_handle};

    use crate::{config::MapConfigManager, flow::session::InMemorySessionStore, secret::EmptySecretsManager};

    use super::*;

    impl ChannelManager {
        pub fn dummy() -> Arc<Self> {
            Arc::new(ChannelManager { 
                config: ConfigManager(MapConfigManager::new()), 
                secrets: SecretsManager(EmptySecretsManager::new()), 
                store: InMemorySessionStore::new(10),
                channels: Arc::new(DashMap::new()), 
                log_config: LogConfig::default(),
                incoming_subscribers: Arc::new(Mutex::new(Vec::new())), 
            })
        }
    }

    #[tokio::test]
    async fn test_register_and_unload() {
        let secrets = SecretsManager(EmptySecretsManager::new());
        let config = ConfigManager(MapConfigManager::new());
        let store =InMemorySessionStore::new(10);

        let mgr = ChannelManager::new(config, secrets, store.clone(), LogConfig::default())
            .await
            .unwrap();

        let (_mock,plugin_handle) = spawn_mock_handle().await;
        let wrapper = PluginWrapper::new(plugin_handle, store, LogConfig::default()).await;
        mgr.register_channel("foo".into(), ManagedChannel { wrapper, cancel:None, poller:None}).await.unwrap();
        assert_eq!(mgr.list_channels(), vec!["foo".to_string()]);
        mgr.unload_channel("foo").await.unwrap();
        assert!(mgr.list_channels().is_empty());
    }
}
