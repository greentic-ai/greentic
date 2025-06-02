// src/channel/manager.rs

use std::{
    collections::HashMap, ffi::{c_char, c_void, CStr}, fmt, path::PathBuf, sync::{Arc, Mutex}, time::Duration
};
use anyhow::{bail, Error, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use tokio::time::sleep;
use tracing::{debug, error, info, trace, warn};

use channel_plugin::{message::ChannelMessage, plugin::{ChannelPlugin, ChannelState, LogLevel, PluginError, PluginLogger}};

use crate::{
    channel::{plugin::{Plugin, PluginEventHandler}, wrapper::PluginWrapper},
    config::ConfigManager,
    secret::SecretsManager, watcher::DirectoryWatcher,
};


/// in src/channel/manager.rs

/// a subscriber gets every incoming ChannelMessage
#[async_trait]
pub trait IncomingHandler: Send + Sync {
    async fn handle_incoming(&self, msg: ChannelMessage);
}


/// Manages all dynamically‐loaded "channels" (.so/.dll) and routes their messages.
#[derive(Clone)]
pub struct ChannelManager {
    config:        ConfigManager,
    secrets:       SecretsManager,
    channels:      Arc<DashMap<String, PluginWrapper>>,
    host_logger:   Arc<HostLogger>,
    incoming_subscribers: Arc<Mutex<Vec<Arc<dyn IncomingHandler>>>>,
}

impl ChannelManager {
    /// Create a new manager that watches `plugins_dir` for channel plugins.
    pub async fn new(
        config: ConfigManager,
        secrets: SecretsManager,
        host_logger: Arc<HostLogger>,
    ) -> Result<Arc<Self>> {
        let me = Arc::new(Self {
            config,
            secrets,
            channels: Arc::new(DashMap::new()),
            host_logger: host_logger.clone(),
            incoming_subscribers: Arc::new(Mutex::new(vec![])),
        });

        // **3)** return your manager — it's already got the subscription in place.
        Ok(me)
    }

    /// Simple diagnostics: channel name → state
    pub fn diagnostics(&self) -> HashMap<String, ChannelState> {
        self.channels
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().state()))
            .collect()
    }

    /// subscribe to incoming messageds
    pub fn subscribe_incoming(&self, h: Arc<dyn IncomingHandler>) {
        self.incoming_subscribers.lock().unwrap().push(h);
    }

    /// Load & start a channel plugin immediately.
    pub fn register_channel(&self, name: String, mut wrapper: PluginWrapper) -> Result<(), PluginError> {
        wrapper.start()?;
        self.channels.insert(name, wrapper);
        Ok(())
    }

    /// Unload & stop a channel by name.
    pub async fn unload_channel(&self, name: &str) -> Result<(), PluginError> {
        if let Some((_, mut wrapper)) = self.channels.remove(name) {
            wrapper.stop()?;
        }
        Ok(())
    }

    /// Start (or restart) a currently‐loaded channel.
    pub async fn start_channel(&self, name: &str) -> Result<(), PluginError> {
        if let Some(mut entry) = self.channels.get_mut(name) {
            entry.value_mut().start()?;
        }
        Ok(())
    }

    /// Get a channel.
    pub fn channel(&self, name: &str) -> Option<PluginWrapper> {
        if let Some(entry) = self.channels.get(name) {
            Some(entry.value().clone())
        } else {
            None
        }

    }

    /// Stop (but keep loaded) a channel.
    pub async fn stop_channel(&self, name: &str) -> Result<(), PluginError> {
        if let Some(mut entry) = self.channels.get_mut(name) {
            entry.value_mut().stop()?;
        }
        Ok(())
    }

    /// Send a message into a running plugin.  
    /// Returns Err if the plugin isn't loaded or send() fails.
    pub fn send_to_channel(
        &self,
        name: &str,
        msg: ChannelMessage,
    ) -> Result<(), PluginError> {
        if let Some(mut wrapper) = self.channels.get_mut(name) {
            wrapper.send(msg)
        } else {
            Err(PluginError::Other(format!("channel `{}` not loaded", name)))
        }
    }

    /// List the names of all loaded channels.
    pub fn list_channels(&self) -> Vec<String> {
        self.channels.iter().map(|kv| kv.key().clone()).collect()
    }

    /// Get all loaded channels.
    pub fn channels(&self) -> Arc<DashMap<String,PluginWrapper>> {
        self.channels.clone()
    }

    /// Start watching the plugins directory, subscribe ourselves,
    /// and spawn the watcher task.
    ///
    /// Returns a JoinHandle so you can abort on shutdown.
    pub async fn start_all(self: Arc<Self>, plugins_dir: PathBuf) -> Result<DirectoryWatcher, Error> {
        // 1) build the watcher
        let watcher = Arc::new(crate::channel::plugin::PluginWatcher::new(plugins_dir.clone()));
        // 2) subscribe us to get add/remove events
        watcher.subscribe(self.clone() as Arc<dyn crate::channel::plugin::PluginEventHandler>).await;
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
            let mut w = kv.value().clone();
            if graceful {
                let _ = w.drain();
            } else {
                let _ = w.stop();
            }
        }
        // If draining, wait them out.
        if graceful {
            for kv in self.channels.iter() {
                let mut w = kv.value().clone();
                let _ = w.wait_until_drained(timeout_ms);
            }
        }
    }
}

#[async_trait]
impl PluginEventHandler for ChannelManager {
    /// Called when a `.so`/`.dll` is added or changed.
    async fn plugin_added_or_reloaded(&self, name: &str, plugin: Arc<Plugin>) -> Result<(), Error> {
        info!("Channel plugin added/reloaded: {}", name);

        // If already present, tear down the old one:
        if let Some(mut old_plugin) = self.channels.get_mut(name) {
            // stopping the channel
            if let Err(e) = old_plugin.stop() {
                info!("Could not stop existing plugin `{}`: {:?}", name, e);
            }
            // Now remove it from the map
            // (we must drop the `RefMut` before calling `remove`)
            drop(old_plugin);
            self.channels.remove(name);
            info!("— replaced existing channel `{}`", name);
        }

        // Wrap + configure:
        let mut wrapper = PluginWrapper::new(plugin);

        // Wire in the host’s logger callback
        let ffi_logger = self.host_logger.clone().as_ffi();
        wrapper.set_logger(ffi_logger);

        // 1) Config values
        let mut cfg_map = HashMap::new();
        for key in wrapper.list_config() {
            if let Some(val) = self.config.0.get(&key).await {
                cfg_map.insert(key.clone(), val.clone());
            }
        }
        wrapper.set_config(cfg_map);

        // 2) Secrets
        let mut sec_map = HashMap::new();
        for key in wrapper.list_secrets() {
            if let Some(tok) = self.secrets.0.get(&key) {
                if let Ok(Some(secret)) = self.secrets.0.reveal(tok).await {
                    sec_map.insert(key.clone(), secret);
                }
            }
        }
        wrapper.set_secrets(sec_map);

        // 3) Start it
        match wrapper.start() {
            Err(e)=>{error!("Could not start plugin `{}`: {:?}",name,e);bail!(e)},
            Ok(_) => {info!("Plugin {} started",name)}
        };

        // 4) Insert into our map
        self.channels.insert(name.to_string(), wrapper.clone());

        // 5) Spawn its polling loop
        let caps = wrapper.capabilities();
        if caps.supports_receiving {
            let channel_name = name.to_string();
            let wrapper = wrapper.clone(); 
            let subs = self.incoming_subscribers.clone();
            tokio::spawn(async move { 
                loop {
                    match wrapper.poll() {
                        Ok(mut msg) => {
                            msg.channel = channel_name.clone();

                            // 1) grab and clone the subscriber list under the lock
                            let handlers: Vec<Arc<dyn IncomingHandler>> = {
                                let guard = subs.lock().unwrap();
                                guard.clone()
                            };
                            // 2) now drop the lock, and call each handler
                            for handler in handlers {
                                handler.handle_incoming(msg.clone()).await;
                            }
                        }
                        Err(e) => {
                            warn!("Poll error on channel `{}`: {:?}", channel_name, e);
                        }
                    }
                    sleep(Duration::from_millis(200)).await;
                }
            });
        }

        Ok(())
    }

    /// Called when a `.so`/`.dll` is removed.
    async fn plugin_removed(&self, name: &str) -> Result<(), Error> {

        if let Some(mut old_plugin) = self.channels.get_mut(name) {
            // stopping the channel
            if let Err(e) = old_plugin.stop() {
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

extern "C" fn host_log_fn(
    _ctx: *mut c_void,
    level: LogLevel,
    context: *const c_char,
    message: *const c_char,
) {
    // Recover your Rust object
    //let logger = unsafe { &*(ctx as *const HostLogger) };
    // Turn C strings into Rust &str
    let ctx_str = unsafe { CStr::from_ptr(context) }.to_string_lossy();
    let msg_str = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    // Dispatch into tracing
    match level {
        LogLevel::Trace    => trace!("{} - {}", ctx_str, msg_str),
        LogLevel::Debug    => debug!("{} - {}", ctx_str, msg_str),
        LogLevel::Info     => info!("{} - {}", ctx_str, msg_str),
        LogLevel::Warn     => warn!("{} - {}", ctx_str, msg_str),
        LogLevel::Error    => error!("{} - {}", ctx_str, msg_str),
        LogLevel::Critical => error!("[CRITICAL] {} - {}", ctx_str, msg_str),
    }
}


/// A logger you hand to each plugin so that
/// plugin.log(...) calls turn into host tracing calls.
#[derive(Clone, Debug)]
pub struct HostLogger;

impl HostLogger {
    pub fn new() -> Arc<Self> {
        Arc::new(HostLogger)
    }

    /// build a PluginLogger without consuming `self`
    pub fn as_ffi(&self) -> PluginLogger {
        PluginLogger {
            ctx: self as *const _ as *mut c_void,
            log_fn: host_log_fn,
        }
    }
}

//
// Tests (bring ChannelPlugin into scope for PluginWrapper methods)
//

#[cfg(test)]
pub mod tests {
    use crate::{config::MapConfigManager, secret::EmptySecretsManager,};

    use super::*;
    use std::{path::PathBuf, sync::Arc, time::SystemTime};
    use channel_plugin::{
        message::{ChannelCapabilities, ChannelMessage},
        plugin::ChannelState,
        PluginHandle,
    };

    impl ChannelManager {
        pub fn dummy() -> Arc<Self> {
            Arc::new(ChannelManager { 
                config: ConfigManager(MapConfigManager::new()), 
                secrets: SecretsManager(EmptySecretsManager::new()), 
                channels: Arc::new(DashMap::new()), 
                host_logger: HostLogger::new(), 
                incoming_subscribers: Arc::new(Mutex::new(Vec::new())), 
            })
        }
    }

    /// A dummy Plugin whose FFI pointers do nothing.
    pub fn make_noop_plugin() -> Arc<Plugin> {
        // All the extern-C functions:
        unsafe extern "C" fn create() -> PluginHandle { std::ptr::null_mut() }
        unsafe extern "C" fn destroy(_: PluginHandle) {}
        unsafe extern "C" fn set_logger(_: PluginHandle, _logger_ptr: PluginLogger) {}
        unsafe extern "C" fn name(_: PluginHandle) -> *mut i8 { std::ptr::null_mut() }
        unsafe extern "C" fn start(_: PluginHandle) -> bool { true }
        unsafe extern "C" fn drain(_: PluginHandle) -> bool { true }
        unsafe extern "C" fn stop(_: PluginHandle) -> bool { true }
        unsafe extern "C" fn wait_until_drained(_: PluginHandle, _: u64) -> bool { true }
        unsafe extern "C" fn poll(_: PluginHandle, _out: *mut ChannelMessage) -> bool { false }
        unsafe extern "C" fn send(_: PluginHandle, _: *const ChannelMessage) -> bool { true }
        unsafe extern "C" fn caps(_: PluginHandle, out: *mut ChannelCapabilities) -> bool {
            if !out.is_null() {
                unsafe { std::ptr::write(out, ChannelCapabilities::default()) };
                true
            } else {
                false
            }
        }
        unsafe extern "C" fn state(_: PluginHandle) -> ChannelState {
            ChannelState::Stopped
        }
        unsafe extern "C" fn set_config(_: PluginHandle, _: *const i8) {}
        unsafe extern "C" fn set_secrets(_: PluginHandle, _: *const i8) {}
        unsafe extern "C" fn list_config(_: PluginHandle) -> *mut i8 { std::ptr::null_mut() }
        unsafe extern "C" fn list_secrets(_: PluginHandle) -> *mut i8 { std::ptr::null_mut() }
        unsafe extern "C" fn free_string(_: *mut i8) {}

        Arc::new(Plugin {
            lib: None,
            handle: unsafe { create() },
            destroy,
            set_logger,
            name,
            start,
            drain,
            stop,
            wait_until_drained,
            poll,
            send,
            caps,
            state,
            set_config,
            set_secrets,
            list_config,
            list_secrets,
            free_string,
            last_modified: SystemTime::now(),
            path: PathBuf::new(),
        })
    }

    #[tokio::test]
    async fn test_register_and_unload() {
        let secrets = SecretsManager(EmptySecretsManager::new());
        let config = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new();
        let ffi_logger  = host_logger.as_ffi(); 
        let mgr = ChannelManager::new(config, secrets, host_logger)
            .await
            .unwrap();

        let plugin = make_noop_plugin();
        let mut wrapper = PluginWrapper::new(plugin.clone());
        wrapper.set_logger(ffi_logger);
        mgr.register_channel("foo".into(), wrapper).unwrap();
        assert_eq!(mgr.list_channels(), vec!["foo".to_string()]);
        mgr.unload_channel("foo").await.unwrap();
        assert!(mgr.list_channels().is_empty());
    }
}
