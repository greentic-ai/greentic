use std::{thread, time::Duration};
use async_trait::async_trait;
// my_plugin/src/lib.rs
use channel_plugin::{message::{CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult, InitParams, InitResult, ListKeysResult, MessageContent, MessageInResult, MessageOutParams, MessageOutResult, NameResult, Participant, StateResult, StopResult}, plugin_runtime::{run, HasStore, PluginHandler}};
use dashmap::DashMap;
use tracing::info;

// Your real plugin type:
#[derive(Default, Clone)]
pub struct MockPlugin {
    state: ChannelState,
    config: DashMap<String,String>,
    secrets: DashMap<String,String>,
}

impl HasStore for MockPlugin {
    fn config_store(&self)  -> &DashMap<String, String> { &self.config}
    fn secret_store(&self)  -> &DashMap<String, String> { &self.secrets }
}

#[async_trait]
impl PluginHandler for MockPlugin {
    async fn init(&mut self, _params: InitParams) -> InitResult{
        info!("[mock] started");
        self.state = ChannelState::RUNNING;
        InitResult{ success: true, error: None }
    }
    fn name(&self) -> NameResult {
        NameResult{name:"mock_inout".to_string()}
    }

    

    fn capabilities(&self) -> CapabilitiesResult {
        CapabilitiesResult { capabilities: ChannelCapabilities {
            name: "mock_inout".into(),
            supports_sending: true,
            supports_receiving: true,
            supports_text: true,
            supports_routing: false,
            /* … */
            ..Default::default()
        }}
    }

    async fn state(&self) -> StateResult { StateResult{state:self.state.clone()} }
    async fn drain(&mut self) -> DrainResult { info!("[mock] drain"); self.state = ChannelState::DRAINING; DrainResult{ success: true, error: None }}
    async fn stop(&mut self) -> StopResult { info!("[mock] stop"); self.state = ChannelState::STOPPED; StopResult{ success: true, error: None } }
    
    fn list_config_keys(&self) -> ListKeysResult{
        ListKeysResult{ required_keys: vec![], optional_keys: vec![] }
    }    

    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult{ required_keys: vec![], optional_keys: vec![] }
    }
    
    async fn send_message(&mut self,  params: MessageOutParams) -> MessageOutResult {
        
        info!("[mock] got a new message {:?}",params.message);
        MessageOutResult{ success: true, error: None }
        
    }
    
    async fn receive_message(&mut self) -> MessageInResult {
        if self.state == ChannelState::RUNNING {
            info!("[mock] receive_message");
            thread::sleep(Duration::from_secs(10));
            // Generate your message here (this example just uses the default)
            let mut msg = ChannelMessage::default();
            msg.channel = "mock_inout".to_string();
            let content = MessageContent::Text{text:"mock says hello".to_string()};
            msg.content = vec![content];
            let participant = Participant{id:"mockingbird".to_string(), display_name: None, channel_specific_id: None };
            msg.from = participant;

            MessageInResult{message:msg, error:false}
        } else {
            // ✳️ Instead of sending default/invalid message with error: true, just wait
            tokio::time::sleep(Duration::from_millis(100)).await;
            // Loop back without emitting a message
            self.receive_message().await
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(MockPlugin::default()).await
}