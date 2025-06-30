


use std::{collections::VecDeque, sync::{Arc, Condvar, Mutex}};

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::oneshot;

use crate::{jsonrpc::{Request, Response}, message::{CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, InitParams, InitResult, ListKeysResult, MessageInResult, MessageOutParams, MessageOutResult, NameResult, StateResult, WaitUntilDrainedParams, WaitUntilDrainedResult}, plugin_actor::{spawn_plugin_actor, Command, PluginHandle}, plugin_runtime::{HasStore, PluginHandler}};



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
    pub fn new() -> Self {
            Self {
                messages: Arc::new(Mutex::new(VecDeque::new())),
                cvar: Arc::new(Condvar::new()),
                outgoing: Arc::new(Mutex::new(vec![])),
                state: Arc::new(Mutex::new(ChannelState::STARTING)),
                config: DashMap::new(),
                secrets: DashMap::new(),
            }
        }

    /// Handy factory that gives you a ready-to-use mock `PluginHandle`
    /// and ignores the event stream.
    pub async fn make_mock_handle() -> PluginHandle {
        // create an Arc so it can be moved into the actor task
        let mock = Arc::new(MockChannel::new());
        mock.get_plugin_handle().await
    }

    /// Spawn an in-process actor for *this* mock channel
    /// and return the corresponding `PluginHandle`.
    pub async fn get_plugin_handle(self: Arc<Self>) -> PluginHandle {
        let (handle, _events) =
            PluginHandle::in_process(self)
                .await
                .expect("Failed to create in-process handle");
        handle
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

impl HasStore for Arc<MockChannel> {
    fn config_store(&self)  -> &DashMap<String, String> { &self.config }
    fn secret_store(&self)  -> &DashMap<String, String> { &self.secrets }
}

#[async_trait]
impl PluginHandler for Arc<MockChannel> {
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

    async fn init(&mut self, _params: InitParams) -> InitResult {
        *self.state.lock().unwrap() = ChannelState::RUNNING;
        InitResult{ success: true, error: None }
    }

    async fn drain(&mut self) {
        *self.state.lock().unwrap() = ChannelState::DRAINING;
    }

    async fn wait_until_drained(&self, _params: WaitUntilDrainedParams) -> WaitUntilDrainedResult {
        WaitUntilDrainedResult{ stopped: true, error: false }
    }

    async fn stop(&mut self) {
        *self.state.lock().unwrap() = ChannelState::STOPPED;
    }
    
    async fn send_message(&mut self, params: MessageOutParams) -> MessageOutResult{

        self.outgoing.lock().unwrap().push(params.message);
        MessageOutResult{ success: true, error: None }
    }
    
    async fn receive_message(&mut self) -> MessageInResult{
        // grab the lock
        let mut guard = self.messages.lock().unwrap();
        // wait while empty
        while guard.is_empty() {
            guard = self.cvar.wait(guard).unwrap();
        }
        // at least one message—pop & return
        MessageInResult{message:guard.pop_front().unwrap()}
    }

}


/// 2. Helper that turns *any* `PluginHandler` into a ready-to-use `PluginHandle`
pub async fn in_process_handle<P: PluginHandler>(plugin: P) -> PluginHandle {
    // identical to your `run()` but without stdin/stdout plumbing
    let (cmd_tx, _event_rx) = spawn_plugin_actor(plugin).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Request, oneshot::Sender<Response>)>(8);

    // forwarder task
    tokio::spawn(async move {
        while let Some((req, rsp_tx)) = rx.recv().await {
            let _ = cmd_tx.send(Command::Call(req, rsp_tx)).await;
        }
    });

    PluginHandle::new(tx,"mock".into())
}

/// 3. Factory you can call from tests or from `make_wrapper`
pub async fn make_mock_handle() -> PluginHandle {
    let mock = Arc::new(MockChannel::new());
    in_process_handle(mock).await 
}


