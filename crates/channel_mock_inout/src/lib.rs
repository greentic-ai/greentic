use std::{thread, time::Duration};
use async_trait::async_trait;
// my_plugin/src/lib.rs
use channel_plugin::{export_plugin, message::{ChannelCapabilities, ChannelMessage, MessageContent, Participant}, plugin::{ChannelPlugin, ChannelState, LogLevel, PluginError, PluginLogger}};
use dashmap::DashMap;

// Your real plugin type:
#[derive(Default)]
pub struct MockPlugin {
    state: ChannelState,
    config: DashMap<String,String>,
    secrets: DashMap<String,String>,
    logger: Option<PluginLogger>,
}
#[async_trait]
impl ChannelPlugin for MockPlugin {
    fn name(&self) -> String {
        "mock_inout".to_string()
    }

    fn set_logger(&mut self, logger: PluginLogger) {
        self.logger = Some(logger);
    }

    fn get_logger(&self) -> Option<PluginLogger> {
        self.logger
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            name: "mock_inout".into(),
            supports_sending: true,
            supports_receiving: true,
            supports_text: true,
            /* … */
            ..Default::default()
        }
    }
    fn set_config(&mut self, config: DashMap<String, String>) { self.config = config; }
    fn set_secrets(&mut self, secrets: DashMap<String, String>) { self.secrets = secrets; }

    fn state(&self) -> ChannelState { self.state.clone() }
    async fn start(&mut self) -> Result<(),PluginError> { self.state = ChannelState::Running; Ok(()) }
    fn drain(&mut self) -> Result<(),PluginError> { self.state = ChannelState::Draining; Ok(()) }
    async fn wait_until_drained(&mut self, _timeout_ms: u64) -> Result<(),PluginError> { self.state = ChannelState::Stopped; Ok(()) }
    async fn stop(&mut self) -> Result<(),PluginError> { self.state = ChannelState::Stopped; Ok(()) }
    
    fn list_config(&self) -> Vec<String> {
        vec!["config".to_string()]
    }    

    fn list_secrets(&self) -> Vec<String> {
        vec!["secret".to_string()]
    }
    
    async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(),PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "mock_out", format!("got a new message {:?}",msg).as_str());
        }
        Ok(())
    }
    
    async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage,PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "mock_in", "receive_message");
        }
        thread::sleep(Duration::from_secs(10));
        // Generate your message here (this example just uses the default)
        let mut msg = ChannelMessage::default();
        msg.channel = "mock_in".to_string();
        let content = MessageContent::Text("mock says hello".to_string());
        msg.content = Some(content);
        let participant = Participant{id:"mockingbird".to_string(), display_name: None, channel_specific_id: None };
        msg.from = participant;

        Ok(msg)
    }
}

export_plugin!(MockPlugin);