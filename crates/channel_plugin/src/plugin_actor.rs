use serde::{Deserialize, Serialize};
use serde_json::json;
use strum_macros::{AsRefStr, Display, EnumString};
use tokio::sync::{mpsc, oneshot};
use anyhow::anyhow;
use crate::channel_client::ChannelClient;
#[cfg(feature = "test-utils")]
use crate::channel_client::InProcChannelClient;
use crate::jsonrpc::{Id, Request, Response};
use crate::message::*;
use crate::plugin_runtime::PluginHandler;

// -----------------------------------------------------------------------------
// Define Commands for actor
// -----------------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq, EnumString, AsRefStr, Display, Serialize, Deserialize)]
#[strum(serialize_all = "camelCase")]
pub enum Method {
    Init,
    Start,
    Drain,
    Stop,
    State,
    Health,
    MessageIn,
    MessageOut,
    SetConfig,
    SetSecrets,
    Capabilities,
    ListConfigKeys,
    ListSecretKeys,
    WaitUntilDrained,
    // Add more as needed
}
#[derive(Debug)]
pub enum Command {
    Init(InitParams, oneshot::Sender<InitResult>),
    Start(InitParams, oneshot::Sender<InitResult>),
    Drain,
    Stop,
    State(oneshot::Sender<StateResult>),
    Health(oneshot::Sender<HealthResult>),
    SendMessage(MessageOutParams, oneshot::Sender<MessageOutResult>),
    ReceiveMessage(oneshot::Sender<MessageInResult>),
    SetConfig(SetConfigParams),
    SetSecrets(SetSecretsParams),
    Capabilities(oneshot::Sender<CapabilitiesResult>),
    WaitUntilDrained(WaitUntilDrainedParams, oneshot::Sender<WaitUntilDrainedResult>),
    Call(Request, oneshot::Sender<Response>),
}


// Event sent from plugin to outside
pub type PluginEvent = (String, Request);

/// Returned when you spawn a plugin in-process.
/// Use it to send JSON-RPC requests to the plugin.
#[derive( Debug)]
pub struct PluginHandle<C>
where
    C: ChannelClient + Clone + Send + Sync + 'static,
{
    /// “Slow-path” control channel (unchanged)
    cmd_tx: mpsc::Sender<Command>,

    /// Message in/out abstraction – could be JSON-RPC, a gRPC stub,
    /// a future NATS client, … whatever implements `ChannelClient`.
    ///
    /// Notice we *clone* the client for every handle to keep
    /// `PluginHandle` itself `Clone`.
    client: C,
    plugin_id: String,
}

impl<C> Clone for PluginHandle<C>
where
    C: ChannelClient + Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            cmd_tx: self.cmd_tx.clone(),
            client: self.client.clone(),   // fresh receiver for every clone
            plugin_id: self.plugin_id.clone(),
        }
    }
}

impl<C> PluginHandle<C>
where
    C: ChannelClient + Clone + Send + Sync + 'static,
{
    pub fn new(
        cmd_tx: mpsc::Sender<Command>,
        client: C,
        plugin_id: String,) -> Self {
        Self{cmd_tx, client,plugin_id}
    }
    

    

    pub fn id(&self) -> &str {
        &self.plugin_id
    }

   

    

    // ---------------------------------------------------------------------
    // Convenience wrappers
    // ---------------------------------------------------------------------

    pub async fn state(&self) -> anyhow::Result<ChannelState> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(Command::State(tx)).await?;
        Ok(rx.await?.state)
    }

    pub async fn send_message(&self, msg: ChannelMessage) -> anyhow::Result<()> {
        self.client.send(msg).await
    }

    pub async fn next_message(&mut self) -> Option<ChannelMessage> {
        // We need a &mut self only because `ChannelClient::next_inbound`
        // takes &mut (to keep an internal receiver).
        self.client.next_inbound().await
    }


    pub async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx.send(Command::Capabilities(tx)).await?;
        Ok(rx.await?.capabilities)
    }

    pub async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx.send(Command::Call(
            Request::call(
                Id::String("listConfigKeys".to_string()),
                Method::ListConfigKeys.to_string(),
                None,
            ),
            tx,
        )).await?;
        let res = rx.await?;
        Ok(serde_json::from_value(res.result.ok_or_else(|| anyhow!("Missing result"))?)?)
    }

    pub async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx.send(Command::Call(
            Request::call(
                Id::String("listSecretKeys".to_string()),
                Method::ListSecretKeys.to_string(),
                None,
            ),
            tx,
        )).await?;
        let res = rx.await?;
        Ok(serde_json::from_value(res.result.ok_or_else(|| anyhow!("Missing result"))?)?)
    }

    pub async fn start(&self, params: InitParams) -> anyhow::Result<InitResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx.send(Command::Start(params, tx)).await?;
        Ok(rx.await?)
    }



    pub async fn drain(&self) -> anyhow::Result<()> {
        self.cmd_tx.send(Command::Drain).await?;
        Ok(())
    }

    pub async fn stop(&self) -> anyhow::Result<()> {
        self.cmd_tx.send(Command::Stop).await?;
        Ok(())
    }

    pub async fn health(&self) -> anyhow::Result<HealthResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx.send(Command::Health(tx)).await?;
        Ok(rx.await?)
    }

    pub async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(Command::WaitUntilDrained(
                WaitUntilDrainedParams { timeout_ms },
                tx,
            ))
            .await?;
        let _ = rx.await?;
        Ok(())
    }


     /* ────────────────────────────────────────────────────────────────────────
     * 1)  In-process actor
     * ──────────────────────────────────────────────────────────────────────── */
    #[cfg(feature = "test-utils")]
    pub async fn in_process<P>(
        plugin: P,
    ) -> anyhow::Result<(PluginHandle<InProcChannelClient>,
                        mpsc::UnboundedReceiver<(String, Request)>)>
    where
        P: PluginHandler + Clone + 'static,
    {
        // ① run the actor → gives us a fully-wired handle
        let handle: PluginHandle<InProcChannelClient> =
            run_plugin_instance(plugin).await;

        // ② dummy “events” receiver (keeps the old tests compiling)
        let (_ev_tx, ev_rx) = mpsc::unbounded_channel::<(String, Request)>();

        Ok((handle, ev_rx))
    }
/* 
    /* ────────────────────────────────────────────────────────────────────────
     * 2)  Child-process (binary on disk)
     * ──────────────────────────────────────────────────────────────────────── */
    pub async fn from_exe<P: AsRef<Path>>(
        exe_path: P,
    ) -> anyhow::Result<Self> {
        let (handle, _ev) = Self::from_exe_with_events(exe_path).await?;
        Ok(handle)
    }

    pub async fn from_exe_with_events<P: AsRef<Path>>(
        exe_path: P,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<PluginEvent>)> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let handle               = spawn_plugin(exe_path, event_tx).await?;
        Ok((handle, event_rx))
    }
*/
    }

// -----------------------------------------------------------------------------
// Launch plugin actor and return channels
// -----------------------------------------------------------------------------
#[cfg(feature = "test-utils")]
pub async fn run_plugin_instance<P>(
    plugin: P,
) -> PluginHandle<InProcChannelClient>
where
    P: PluginHandler,
{
    use std::sync::Arc;

    use tokio::sync::Mutex;

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
    let (msg_tx,  msg_rx)   = mpsc::channel::<ChannelMessage>(32);
    let name = plugin.name().name.clone();
    let mut plugin_clone = plugin.clone();
    let mut plugin_poll = plugin;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                /* ───────────── incoming RPC / control commands ───────────── */
                Some(cmd) = cmd_rx.recv() => { 
                    match cmd {
                        Command::Init(p, tx) => {
                            println!("@@@ REMOVE init: {:?}",p);
                            let _ = tx.send(plugin_clone.start(p).await);
                        },
                        Command::Start(p, tx) => {
                            println!("@@@ REMOVE Start: {:?}",p);
                            let _ = tx.send(plugin_clone.start(p).await);
                        },
                        Command::Drain => {
                            println!("@@@ REMOVE drain: ");
                            plugin_clone.drain().await;
                        },
                        Command::Stop => {
                            println!("@@@ REMOVE drain: ");
                            plugin_clone.stop().await;
                        },
                        Command::State(tx) => {
                            println!("@@@ REMOVE drain: ");
                            let _ = tx.send(plugin_clone.state().await);
                        },
                        Command::Health(tx) => {
                            println!("@@@ REMOVE health: ");
                            let _ = tx.send(plugin_clone.health().await);
                        },
                        Command::SendMessage(p, tx) => {
                            println!("@@@ REMOVE send: {:?} ",p);
                            let _ = tx.send(plugin_clone.send_message(p).await);
                        },
                        Command::ReceiveMessage(tx) => {
                            println!("@@@ REMOVE receive");
                            let _ = tx.send(plugin_clone.receive_message().await);
                        },
                        Command::Capabilities(tx) => {
                            println!("@@@ REMOVE capabilities: ");
                            let _ = tx.send(plugin_clone.capabilities());
                        },
                        Command::SetConfig(p) => {
                            println!("@@@ REMOVE config: {:?}",p);
                            let _ = plugin_clone.set_config(p).await;
                        },
                        Command::SetSecrets(p) => {
                            println!("@@@ REMOVE secret: {:?}",p);
                            let _ = plugin_clone.set_secrets(p).await;
                        },
                        Command::WaitUntilDrained(p, tx) => {
                            let _ = tx.send(plugin_clone.wait_until_drained(p).await);
                        },
                        Command::Call(req, tx) => {
                            println!("@@@ REMOVE call: {:?}",req);
                            let response = handle_internal_request(&mut plugin_clone, req).await;
                            let _ = tx.send(response);
                        },
                    }
                },
               
            }
        }
    });
        /* ────────────────── 2. message-poller ─────────────── */
    tokio::spawn(async move {
        loop {
            let res = plugin_poll.receive_message().await;
            println!("@@@ REMOVE receive");
            // ignore back-pressure – test mocks never flood
            if msg_tx.send(res.message).await.is_err() {
                break;               // all handles dropped → exit
            }
        }
    });
    let client = InProcChannelClient {
        cmd_tx: cmd_tx.clone(),
        in_rx : Arc::new(Mutex::new(msg_rx)),
    };

    PluginHandle::new(cmd_tx, client, name)
}




// -----------------------------------------------------------------------------
// Request handler
// -----------------------------------------------------------------------------

/// Dispatch a single JSON-RPC request **inside** an actor task
/// and return the matching `Response` so the caller can decide
/// what to do with it (e.g. forward to stdout or a channel).
#[cfg(feature = "test-utils")]
async fn handle_internal_request<P: PluginHandler>(
    plugin: &mut P,
    req: Request,
) -> Response {
    use serde_json::Value;

    // Convenience helpers ----------------------------------------------------
    fn ok_null(id: Id) -> Response {
        Response::success(id, json!(null))
    }

    fn err(id: Id, code: i64, msg: &str, data: Option<Value>) -> Response {
        Response::fail(id, code, msg, data)
    }

    // ------------------------------------------------------------------------
    match req.method.parse::<Method>() {
        // ─────────────── notifications / calls the runner understands ───────
        Ok(Method::Init) => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<InitParams>(v).ok())
            {
                Some(p) => {
                    let res = plugin.start(p).await;
                    if res.success {
                        ok_null(id)
                    } else {
                        err(id, -32000, "Init failed", Some(json!(res.error)))
                    }
                }
                None => err(id, -32602, "Invalid params", None),
            }
        }

         Ok(Method::Start) => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<InitParams>(v).ok()) {
                Some(p) => Response::success(id, json!(plugin.start(p).await)),
                None    => err(id, -32602, "Invalid params", None),
            }
        }

        Ok(Method::WaitUntilDrained) => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<WaitUntilDrainedParams>(v).ok()) {
                Some(p) => Response::success(id, json!(plugin.wait_until_drained(p).await)),
                None    => err(id, -32602, "Invalid params", None),
            }
        }

        Ok(Method::Stop) => {
            let id = req.id.unwrap_or(Id::Null);
            plugin.stop().await;
            ok_null(id)
        }


        Ok(Method::Drain) => {
            let id = req.id.unwrap_or(Id::Null);
            plugin.drain().await;
            ok_null(id)
        }

        Ok(Method::Capabilities) => {
            let id = req.id.unwrap_or(Id::Null);
            Response::success(id, json!(plugin.capabilities()))
        }

        Ok(Method::MessageIn) => {
            let id = req.id.unwrap_or(Id::Null);
            Response::success(id, json!(plugin.receive_message().await))
        }

        Ok(Method::MessageOut) => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<MessageOutParams>(v).ok())
            {
                Some(p) => Response::success(id, json!(plugin.send_message(p).await)),
                None    => err(id, -32602, "Invalid params", None),
            }
        }

        Ok(Method::Health) => {
            let id = req.id.unwrap_or(Id::Null);
            Response::success(id, json!(plugin.health().await))
        }

        Ok(Method::State) => {
            let id = req.id.unwrap_or(Id::Null);
            Response::success(id, json!(plugin.state().await))
        }

        Ok(Method::ListConfigKeys) => {
            let id = req.id.unwrap_or(Id::Null);
            Response::success(id, json!(plugin.list_config_keys()))
        }

        Ok(Method::ListSecretKeys) => {
            let id = req.id.unwrap_or(Id::Null);
            Response::success(id, json!(plugin.list_secret_keys()))
        }

        Ok(Method::SetConfig) => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<SetConfigParams>(v).ok())
            {
                Some(p) => Response::success(id, json!(plugin.set_config(p).await)),
                None    => err(id, -32602, "Invalid params", None),
            }
        }

        Ok(Method::SetSecrets) => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<SetSecretsParams>(v).ok())
            {
                Some(p) => Response::success(id, json!(plugin.set_secrets(p).await)),
                None    => err(id, -32602, "Invalid params", None),
            }
        }

        // ─────────────── unknown / unsupported method ───────────────────────
        method => {
            use tracing::error;

            let id = req.id.unwrap_or(Id::Null);
            error!("Plugin is asking for methods which do not exist: {:?}",method);
            err(id, -32601, "Method not found", None)
        }
    }
}


#[cfg(test)]
mod tests {
    use crate::plugin_runtime::{HasStore, PluginHandler, VERSION};

    use super::*;
    use async_trait::async_trait;
    use dashmap::DashMap;

    #[derive(Clone, Default)]
    struct ActorMockPlugin{
        config: DashMap<String, String>,
        secrets: DashMap<String, String>,
    }

    impl HasStore for ActorMockPlugin{
        fn config_store(&self)  -> &DashMap<String, String> { &self.config }
        fn secret_store(&self)  -> &DashMap<String, String> { &self.secrets }
    }

    #[async_trait]
    impl PluginHandler for ActorMockPlugin {
        async fn start(&mut self, _params: InitParams) -> InitResult {
            InitResult { success: true, error: None }
        }
        fn name(&self) -> NameResult { NameResult{name:"mock".to_string()} }
        async fn init(&mut self, _params: InitParams) -> InitResult { InitResult { success: true, error: None }}
        fn list_config_keys(&self) -> ListKeysResult {ListKeysResult{required_keys:vec![],optional_keys:vec![]}}
        fn list_secret_keys(&self) -> ListKeysResult {ListKeysResult{required_keys:vec![],optional_keys:vec![]}}
        fn capabilities(&self) -> CapabilitiesResult {CapabilitiesResult { capabilities: ChannelCapabilities::default()}}
        async fn drain(&mut self) {}
        async fn stop(&mut self) {}
        async fn state(&self) -> StateResult {
            StateResult { state: ChannelState::STOPPED }
        }
        async fn health(&self) -> HealthResult {
            HealthResult { healthy: true, reason: None }
        }
        async fn send_message(&mut self, _params: MessageOutParams) -> MessageOutResult {
            MessageOutResult { success: true, error: None }
        }
        async fn receive_message(&mut self) -> MessageInResult {
            MessageInResult {
                message: ChannelMessage {
                    from: Participant::new("user".into(), None, None),
                    to: vec![Participant::new("bot".into(), None, None)],
                    content: vec![MessageContent::Text{text:"hello".into()}],
                    ..Default::default()
                },
                error: false,
            }
        }
    }

    #[tokio::test]
    async fn test_plugin_handle_capabilities_and_state() {
        let (plugin, mut ev_rx) = PluginHandle::<InProcChannelClient>::in_process(ActorMockPlugin::default()).await.unwrap();

        // State should return "ok"
        let state = plugin.state().await.unwrap();
        assert_eq!(state, ChannelState::STOPPED);

        // Health should be ok
        let health = plugin.health().await.unwrap();
        assert!(health.healthy);

        // Drain and Stop are fire-and-forget (should not panic)
        plugin.drain().await.unwrap();
        plugin.stop().await.unwrap();

        // Receive message is pushed to events
        let (_plugin_id, req) = ev_rx.recv().await.expect("expected messageIn");
        assert_eq!(req.method, "messageIn");
    }

    #[tokio::test]
    async fn test_plugin_handle_send_message() {
        let (plugin, _ev_rx) = PluginHandle::<InProcChannelClient>::in_process(ActorMockPlugin::default()).await.unwrap();

        let msg = MessageOutParams {
            message: ChannelMessage {
                from: Participant::new("bot".into(), None, None),
                to: vec![Participant::new("user".into(), None, None)],
                content: vec![MessageContent::Text{text:"hello".into()}],
                ..Default::default()
            },
        };

        let _ = plugin.send_message(msg.message).await.expect("could not send");
    }

    #[tokio::test]
    async fn test_plugin_handle_start() {
        let (plugin, _ev_rx) = PluginHandle::<InProcChannelClient>::in_process(ActorMockPlugin::default()).await.unwrap();

        let params = InitParams {
            version: VERSION.to_string(),
            config: vec![],
            secrets: vec![],
            log_level: LogLevel::Info,
            log_dir: None,
            otel_endpoint: None,
        };

        let result = plugin.start(params).await.unwrap();
        assert!(result.success);
    }


    #[tokio::test]
    async fn test_plugin_handle_receive_message_rpc() {
        // fresh mock
        let (mut plugin, _ev_rx) =
            PluginHandle::<InProcChannelClient>::in_process(ActorMockPlugin::default()).await.unwrap();

        // ActorMockPlugin::receive_message always returns the static "hello"
        let msg_in = plugin.next_message().await.expect("RPC failed");

        assert_eq!(
            msg_in.content[..],
            [MessageContent::Text { text: "hello".into() }],
            "expected the hard-coded text returned by the mock"
        );
    }

    #[tokio::test]
    async fn test_plugin_wait_until_drained() {
        let (plugin, _ev_rx) =
            PluginHandle::<InProcChannelClient>::in_process(ActorMockPlugin::default()).await.unwrap();

        // fire-and-forget drain
        plugin.drain().await.unwrap();

        // should complete without timing out
        plugin.wait_until_drained(250).await.unwrap();
    }

    #[tokio::test]
    async fn test_plugin_list_config_keys_and_secrets() {
        let (plugin, _ev_rx) =
            PluginHandle::<InProcChannelClient>::in_process(ActorMockPlugin::default()).await.unwrap();

        let cfg = plugin.list_config_keys().await.unwrap();
        let sec = plugin.list_secret_keys().await.unwrap();

        assert!(cfg.required_keys.is_empty());
        assert!(cfg.optional_keys.is_empty());
        assert!(sec.required_keys.is_empty());
        assert!(sec.optional_keys.is_empty());
    }
}
