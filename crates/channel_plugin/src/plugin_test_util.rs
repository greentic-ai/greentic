


use std::{sync::Arc};
use tokio::sync::{mpsc::{self, unbounded_channel, UnboundedReceiver, UnboundedSender}, Mutex};
use async_trait::async_trait;
use dashmap::DashMap;

use crate::{channel_client::ChannelClient, message::{CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, InitParams, InitResult, ListKeysResult, MessageInResult, MessageOutParams, MessageOutResult, NameResult, StateResult, WaitUntilDrainedParams, WaitUntilDrainedResult}, plugin_actor::{Command, PluginHandle}, plugin_runtime::{HasStore, PluginHandler}};



pub struct MockChannel {
    // queue for incoming
    in_rx: Arc<Mutex<Option<UnboundedReceiver<ChannelMessage>>>>,
    in_tx: UnboundedSender<ChannelMessage>,
    outgoing: Arc<Mutex<Vec<ChannelMessage>>>,
    state: Arc<Mutex<ChannelState>>,
    config: DashMap<String, String>,
    secrets: DashMap<String, String>,
}


impl Clone for MockChannel {
    fn clone(&self) -> Self {
        Self {
            in_tx: self.in_tx.clone(),
            in_rx: Arc::clone(&self.in_rx),
            outgoing: Arc::clone(&self.outgoing),
            state: self.state.clone(),
            config: self.config.clone(),
            secrets: self.secrets.clone(),
        }
    }
}

impl MockChannel {
    pub fn new() -> Self {
        let (in_tx, in_rx) = unbounded_channel();
            Self {
                in_tx,
                in_rx: Arc::new(Mutex::new(Some(in_rx))),
                outgoing: Arc::new(Mutex::new(vec![])),
                state: Arc::new(Mutex::new(ChannelState::STOPPED)),
                config: DashMap::new(),
                secrets: DashMap::new(),
            }
        }

        /* 
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
    */
    /// Inject an incoming message and wake any pollers.
    pub async fn inject(&self, msg: ChannelMessage) {
        println!("@@@ REMOVE: inject {:?}",msg);
        let _ = self.in_tx.send(msg);
    }

    pub async fn sent_messages(&self) -> Vec<ChannelMessage> {
        self.outgoing.lock().await.iter().cloned().collect()
    }

}

#[async_trait]
impl ChannelClient for Arc<MockChannel> {
    async fn send(&self, msg: ChannelMessage) -> anyhow::Result<()> {
        // behave exactly like real plugins: push to `outgoing`
        self.outgoing.lock().await.push(msg);
        Ok(())
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        // identical to old receive_message()
        let mut guard = self.in_rx.lock().await;
        let rx = guard
            .as_mut()
            .expect("next_inbound already taken by another instance");
        rx.recv().await
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
        StateResult{state:*self.state.lock().await}
    }

    async fn init(&mut self, _params: InitParams) -> InitResult {
        *self.state.lock().await = ChannelState::RUNNING;
        InitResult{ success: true, error: None }
    }

    async fn drain(&mut self) {
        *self.state.lock().await = ChannelState::DRAINING;
    }

    async fn wait_until_drained(&self, _params: WaitUntilDrainedParams) -> WaitUntilDrainedResult {
        WaitUntilDrainedResult{ stopped: true, error: false }
    }

    async fn stop(&mut self) {
        *self.state.lock().await = ChannelState::STOPPED;
    }
    
    async fn send_message(&mut self, params: MessageOutParams) -> MessageOutResult{

        self.outgoing.lock().await.push(params.message);
        MessageOutResult{ success: true, error: None }
    }
    
    async fn receive_message(&mut self) -> MessageInResult{
        let mut guard = self.in_rx.lock().await;
        let rx = guard
            .as_mut()
            .expect("receive_message already taken by another instance");
        match rx.recv().await {
            Some(msg) => MessageInResult { message: msg, error: false },
            None => MessageInResult { message: Default::default(), error: true },
        }
    }

}
/* 

/// 2. Helper that turns *any* `PluginHandler` into a ready-to-use `PluginHandle`
pub async fn in_process_handle<P: PluginHandler>(plugin: P) -> PluginHandle {
    // identical to your `run()` but without stdin/stdout plumbing
    let (cmd_tx, msg_rx) = run_plugin_instance(plugin).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Request, oneshot::Sender<Response>)>(8);

    // forwarder task
    tokio::spawn(async move {
        while let Some((req, rsp_tx)) = rx.recv().await {
            let _ = cmd_tx.send(Command::Call(req, rsp_tx)).await;
        }
    });

    PluginHandle::new(tx,"mock".into())
}
*/
pub async fn spawn_mock_handle() -> PluginHandle<Arc<MockChannel>> {
    // (1) the control channel – only the commands you still need
    let (cmd_tx, _cmd_rx) = mpsc::channel::<Command>(32);

    // (2) the channel-client abstraction
    let mock = Arc::new(MockChannel::new());

    // (3) DONE – you don’t need the old msg/evt plumbing for tests
    PluginHandle::new(cmd_tx, mock.clone(), "mock".into())
}


#[cfg(any(test, feature = "test-utils"))]
mod tests {

    use chrono::Utc;

    use crate::message::{MessageContent, Participant};

    use super::*;


    pub async fn fresh_mock() -> (Arc<MockChannel>, PluginHandle<Arc<MockChannel>>) {
        let mock   = Arc::new(MockChannel::new());
        let handle = PluginHandle::new(
            mpsc::channel::<Command>(32).0,   // drop rx in tests
            mock.clone(),
            "mock".into(),
        );
        (mock, handle)
    }

    #[tokio::test]
    async fn capabilities_and_metadata() {
        let (_mock, handle) = fresh_mock().await;

        // capabilities ----------------------------------------------------------
        let caps = handle.capabilities().await.expect("cap call failed");
        assert_eq!(caps.name, "mock");
        assert!(caps.supports_sending && caps.supports_receiving);

        // listConfigKeys / listSecretKeys ---------------------------------------
        let cfg  = handle.list_config_keys().await.expect("cfg keys");
        let secr = handle.list_secret_keys().await.expect("sec keys");
        assert!(cfg.required_keys.is_empty() && cfg.optional_keys.is_empty());
        assert!(secr.required_keys.is_empty() && secr.optional_keys.is_empty());
    }

    #[tokio::test]
    async fn state_lifecycle_and_health() {
        let (_mock, handle) = fresh_mock().await;

        // initial state is STARTING ---------------------------------------------
        let mut state = handle.state().await.expect("state call");
        assert_eq!(state, ChannelState::STOPPED);

        // start → RUNNING --------------------------------------------------------
        let res = handle.start(InitParams::default()).await.expect("start");
        assert!(res.success);
        state = handle.state().await.expect("state call");
        assert_eq!(state, ChannelState::RUNNING);

        // drain + waitUntilDrained ----------------------------------------------
        handle.drain().await.expect("drain");
        handle
            .wait_until_drained(250)
            .await
            .expect("waitUntilDrained");

        // stop → STOPPED ---------------------------------------------------------
        handle.stop().await.expect("stop");
        state = handle.state().await.expect("state call");
        assert_eq!(state, ChannelState::STOPPED);

        // health should always be healthy in mock -------------------------------
        let health = handle.health().await.expect("health");
        assert!(health.healthy);
    }

    #[tokio::test]
    async fn send_and_receive_roundtrip() {
        // keep a reference to the mock so we can inject
        let (mock, mut handle) = fresh_mock().await;

        // 1. sendMessage ---------------------------------------------------------
        let out_msg = ChannelMessage {
            from: Participant::new("alice".to_string(), None, None),
            to: vec![Participant::new("bob".to_string(), None, None)],
            content: vec![MessageContent::Text {
                text: "ping".into(),
            }],
            timestamp: Utc::now().to_rfc3339(),
            ..Default::default()
        };
        let ok = handle
            .send_message(out_msg.clone()).await.expect("send");

        // the underlying mock should have recorded it
        assert_eq!(mock.sent_messages().await, vec![out_msg]);

        // 2. receiveMessage (via RPC) -------------------------------------------
        let in_msg = ChannelMessage {
            from: Participant::new("bob".to_string(), None, None),
            to: vec![Participant::new("alice".to_string(), None, None)],
            content: vec![MessageContent::Text {
                text: "pong".into(),
            }],
            timestamp: Utc::now().to_rfc3339(),
            ..Default::default()
        };
        mock.inject(in_msg.clone()).await;

        let got = handle.next_message().await.expect("next msg");
        assert_eq!(got, in_msg);
    }

    #[tokio::test]
    async fn internal_wait_until_drained_path() {
        // Uses PluginHandler API directly through RPC
        let (mock, handle) = fresh_mock().await;
        // No messages → should immediately succeed
        handle.wait_until_drained(100).await.expect("drained");
        let _ = mock.state;
    }
}


