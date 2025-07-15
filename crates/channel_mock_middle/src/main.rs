use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

use async_trait::async_trait;
use channel_plugin::{
    message::{
        CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult,
        InitParams, InitResult, ListKeysResult, MessageInResult, MessageOutParams,
        MessageOutResult, NameResult, StateResult, StopResult,
    },
    plugin_runtime::{HasStore, PluginHandler, run},
};
use dashmap::DashMap;
use tracing::info;

// Your real plugin type:
#[derive(Default, Clone)]
pub struct MockPlugin {
    state: Arc<Mutex<ChannelState>>,
    config: DashMap<String, String>,
    secrets: DashMap<String, String>,
    queue: Arc<(Mutex<VecDeque<ChannelMessage>>, Condvar)>,
}

impl HasStore for MockPlugin {
    fn config_store(&self) -> &DashMap<String, String> {
        &self.config
    }
    fn secret_store(&self) -> &DashMap<String, String> {
        &self.secrets
    }
}

#[async_trait]
impl PluginHandler for MockPlugin {
    async fn init(&mut self, _params: InitParams) -> InitResult {
        *self.state.lock().unwrap() = ChannelState::RUNNING;
        InitResult {
            success: true,
            error: None,
        }
    }
    fn name(&self) -> NameResult {
        NameResult {
            name: "mock_middle".to_string(),
        }
    }

    fn capabilities(&self) -> CapabilitiesResult {
        CapabilitiesResult {
            capabilities: ChannelCapabilities {
                name: "mock_middle".into(),
                supports_sending: true,
                supports_receiving: true,
                supports_text: true,
                supports_routing: false,
                /* … */
                ..Default::default()
            },
        }
    }
    async fn state(&self) -> StateResult {
        StateResult {
            state: self.state.lock().unwrap().clone(),
        }
    }
    async fn drain(&mut self) -> DrainResult {
        *self.state.lock().unwrap() = ChannelState::DRAINING;
        DrainResult {
            success: true,
            error: None,
        }
    }
    async fn stop(&mut self) -> StopResult {
        *self.state.lock().unwrap() = ChannelState::STOPPED;
        StopResult {
            success: true,
            error: None,
        }
    }

    fn list_config_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![],
            optional_keys: vec![],
        }
    }

    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![],
            optional_keys: vec![],
        }
    }

    async fn send_message(&mut self, params: MessageOutParams) -> MessageOutResult {
        let msg = params.message;
        info!("[mock-middle] enqueueing message {:?}", msg);
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        q.push_back(msg);
        cvar.notify_one();
        MessageOutResult {
            success: true,
            error: None,
        }
    }

    async fn receive_message(&mut self) -> MessageInResult {
        info!("[mock-middle] polling queue");
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap();
        // wait until there is something in the queue
        while q.is_empty() {
            q = cvar.wait(q).unwrap();
        }
        // we know it’s non-empty now
        MessageInResult {
            message: q.pop_front().unwrap(),
            error: false,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(MockPlugin::default()).await
}
