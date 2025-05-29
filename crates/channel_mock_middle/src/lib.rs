use std::{collections::{HashMap, VecDeque}, sync::{Arc, Condvar, Mutex}};

// my_plugin/src/lib.rs
use channel_plugin::{export_plugin, message::{ChannelCapabilities, ChannelMessage}, plugin::{ChannelPlugin, ChannelState, LogLevel, PluginError, PluginLogger}};

// Your real plugin type:
#[derive(Default)]
pub struct MockPlugin {
    state: ChannelState,
    config: HashMap<String,String>,
    secrets: HashMap<String,String>,
    logger: Option<PluginLogger>,
    queue: Arc<(Mutex<VecDeque<ChannelMessage>>, Condvar)>,
}

impl ChannelPlugin for MockPlugin {
    fn name(&self) -> String {
        "mock_middle".to_string()
    }

    fn set_logger(&mut self, logger: PluginLogger) {
        self.logger = Some(logger);
    }
    

    fn send(&mut self, msg: ChannelMessage) -> anyhow::Result<(), PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "mock_middle", &format!("enqueueing message {:?}", msg));
        }
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.push_back(msg);
        cvar.notify_one();
        Ok(())
    }

    fn poll(&self) -> anyhow::Result<ChannelMessage, PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "mock_middle", "polling queue");
        }
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        // wait until there is something in the queue
        while q.is_empty() {
            q = cvar.wait(q).unwrap();
        }
        // we know it’s non-empty now
        Ok(q.pop_front().unwrap())
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            name: "mock_middle".into(),
            supports_sending: true,
            supports_receiving: true,
            supports_text: true,
            /* … */
            ..Default::default()
        }
    }
    fn set_config(&mut self, config: std::collections::HashMap<String, String>) { self.config = config; }
    fn set_secrets(&mut self, secrets: std::collections::HashMap<String, String>) { self.secrets = secrets; }

    fn state(&self) -> ChannelState { self.state.clone() }
    fn start(&mut self) -> Result<(),PluginError> { self.state = ChannelState::Running; Ok(()) }
    fn drain(&mut self) -> Result<(),PluginError> { self.state = ChannelState::Draining; Ok(()) }
    fn wait_until_drained(&mut self, _timeout_ms: u64) -> Result<(),PluginError> { self.state = ChannelState::Stopped; Ok(()) }
    fn stop(&mut self) -> Result<(),PluginError> { self.state = ChannelState::Stopped; Ok(()) }
    
    fn list_config(&self) -> Vec<String> {
        vec!["config".to_string()]
    }    

    fn list_secrets(&self) -> Vec<String> {
        vec!["secret".to_string()]
    }
}

export_plugin!(MockPlugin);