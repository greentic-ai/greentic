use std::path::Path;

use crate::channel_client::{ChannelClient, ChannelClientType, RpcChannelClient};
use crate::control_client::{ControlClient, ControlClientType, RpcControlClient};
use crate::jsonrpc::Id;
use crate::jsonrpc::{Message, Request, Response};
use crate::message::*;
use crate::plugin_runtime::{HasStore, PluginHandler};
use anyhow::Result;
use dashmap::DashMap;
use serde_json::json;
use std::sync::Arc;
use strum_macros::{Display, EnumString};
use tokio::sync::{mpsc, oneshot};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command as TokioCommand,
    sync::broadcast,
};
// -----------------------------------------------------------------------------
// Define Commands for actor
// -----------------------------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq, EnumString, Display)]
pub enum Method {
    #[strum(serialize = "name")]
    Name,
    #[strum(serialize = "init")]
    Init,
    #[strum(serialize = "start")]
    Start,
    #[strum(serialize = "drain")]
    Drain,
    #[strum(serialize = "stop")]
    Stop,
    #[strum(serialize = "state")]
    State,
    #[strum(serialize = "health")]
    Health,
    #[strum(serialize = "messageIn")]
    MessageIn,
    #[strum(serialize = "messageOut")]
    MessageOut,
    #[strum(serialize = "setConfig")]
    SetConfig,
    #[strum(serialize = "setSecrets")]
    SetSecrets,
    #[strum(serialize = "capabilities")]
    Capabilities,
    #[strum(serialize = "listConfigKeys")]
    ListConfigKeys,
    #[strum(serialize = "listSecretKeys")]
    ListSecretKeys,
    #[strum(serialize = "waitUntilDrained")]
    WaitUntilDrained,
}

#[derive(Debug)]
pub enum Command {
    Name,
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
    WaitUntilDrained(
        WaitUntilDrainedParams,
        oneshot::Sender<WaitUntilDrainedResult>,
    ),
    Call(Request, oneshot::Sender<Response>),
}

// Event sent from plugin to outside
pub type PluginEvent = (String, Request);
/// Returned when you spawn a plugin in-process.
/// Use it to send JSON-RPC requests to the plugin.
#[derive(Debug, Clone)]
pub struct PluginHandle {
    /// Message in/out abstraction – could be JSON-RPC, a gRPC stub,
    /// a future NATS client, … whatever implements `ChannelClient`.
    ///
    /// Notice we *clone* the client for every handle to keep
    /// `PluginHandle` itself `Clone`.
    client: ChannelClient,
    control: ControlClient,
    name: String,
    capabilities: ChannelCapabilities,
    list_config_keys: ListKeysResult,
    list_secret_keys: ListKeysResult,
}

impl PluginHandle {
    pub async fn new(client: ChannelClient, control: ControlClient) -> Self {
        let name = control
            .name()
            .await
            .expect("Could not retrieve name from plugin");
        let capabilities = control
            .capabilities()
            .await
            .expect("Could not retrieve capabilties from plugin");
        let list_config_keys = control
            .list_config_keys()
            .await
            .expect("Could not retrieve config keys from plugin");
        let list_secret_keys = control
            .list_secret_keys()
            .await
            .expect("Could not retrieve secret keys from plugin");

        Self {
            client,
            control,
            name,
            capabilities,
            list_config_keys,
            list_secret_keys,
        }
    }

    pub fn channel_client(&self) -> ChannelClient {
        self.client.clone()
    }

    pub fn control_client(&self) -> ControlClient {
        self.control.clone()
    }

    pub fn name(&self) -> String {
        self.name.clone()
    }

    // ---------------------------------------------------------------------
    // Convenience wrappers
    // ---------------------------------------------------------------------

    pub async fn state(&self) -> anyhow::Result<ChannelState> {
        self.control.state().await
    }

    pub async fn send_message(&self, msg: ChannelMessage) -> anyhow::Result<()> {
        self.client.send(msg).await
    }

    pub async fn next_message(&mut self) -> Option<ChannelMessage> {
        // We need a &mut self only because `ChannelClient::next_inbound`
        // takes &mut (to keep an internal receiver).
        self.client.next_inbound().await
    }

    pub fn capabilities(&self) -> ChannelCapabilities {
        self.capabilities.clone()
    }

    pub fn list_config_keys(&self) -> ListKeysResult {
        self.list_config_keys.clone()
    }

    pub fn list_secret_keys(&self) -> ListKeysResult {
        self.list_secret_keys.clone()
    }

    pub async fn start(&self, params: InitParams) -> anyhow::Result<InitResult> {
        self.control.start(params).await
    }

    pub async fn drain(&self) -> anyhow::Result<DrainResult> {
        self.control.drain().await
    }

    pub async fn stop(&self) -> anyhow::Result<StopResult> {
        self.control.stop().await
    }

    pub async fn health(&self) -> anyhow::Result<HealthResult> {
        self.control.health().await
    }

    pub async fn wait_until_drained(
        &self,
        timeout_ms: u64,
    ) -> anyhow::Result<WaitUntilDrainedResult> {
        self.control.wait_until_drained(timeout_ms).await
    }

    /* ────────────────────────────────────────────────────────────────────────
     * 1)  In-process actor
     * ──────────────────────────────────────────────────────────────────────── */
    pub async fn in_process<P>(
        plugin: P,
    ) -> anyhow::Result<(PluginHandle, mpsc::UnboundedReceiver<(String, Request)>)>
    where
        Arc<P>: PluginHandler + ChannelClientType + HasStore + Send + Sync + 'static,
    {
        // ① run the actor → gives us a fully-wired handle
        let handle: PluginHandle = run_plugin_instance(Arc::new(plugin)).await;

        // ② dummy “events” receiver (keeps the old tests compiling)
        let (_ev_tx, ev_rx) = mpsc::unbounded_channel::<(String, Request)>();

        Ok((handle, ev_rx))
    }
}

pub async fn spawn_rpc_plugin<P>(exe: P) -> Result<PluginHandle>
where
    P: AsRef<Path>,
{
    // ── launch the child process ──────────────────────────────────────
    let mut child = TokioCommand::new(exe.as_ref())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("failed to open plugin stdin");
    let stdout = child.stdout.take().expect("failed to open plugin stdout");

    // ── JSON-RPC actor queue ──────────────────────────────────────────
    let (rpc_tx, mut rpc_rx) = mpsc::channel::<(Request, oneshot::Sender<Response>)>(32);

    // broadcast for inbound ChannelMessage events (`messageIn`)
    let (msg_tx, _) = broadcast::channel::<ChannelMessage>(32);

    // map of <id → responder> for inflight calls
    let inflight: Arc<DashMap<String, oneshot::Sender<Response>>> = Arc::new(DashMap::new());

    // ── task: forward (Request, rsp_tx) → child.stdin ─────────────────
    {
        let inflight = Arc::clone(&inflight);
        tokio::spawn(async move {
            while let Some((req, rsp_tx)) = rpc_rx.recv().await {
                // remember the responder
                if let Some(id) = &req.id {
                    inflight.insert(serde_json::to_string(id).unwrap(), rsp_tx);
                }
                // write line to the plugin
                if stdin
                    .write_all(serde_json::to_string(&req).unwrap().as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                let _ = stdin.write_all(b"\n").await;
                let _ = stdin.flush().await;
            }
        });
    }

    // ── task: read child.stdout → route Response | messageIn ──────────
    {
        let inflight = Arc::clone(&inflight);
        let msg_tx_clone = msg_tx.clone();

        tokio::spawn(async move {
            let mut rdr = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = rdr.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Message>(&line) {
                    Ok(Message::Response(rsp)) => {
                        let key = serde_json::to_string(&rsp.id).unwrap();
                        if let Some((_, tx_rsp)) = inflight.remove(&key) {
                            let _ = tx_rsp.send(rsp);
                        }
                    }
                    // plugin sent a *request* (notification) – usually `"messageIn"`
                    Ok(Message::Request(req)) if req.method == Method::MessageIn.to_string() => {
                        if let Some(p) = req.params {
                            if let Ok(r) = serde_json::from_value::<MessageInResult>(p) {
                                let _ = msg_tx_clone.send(r.message);
                            }
                        }
                    }
                    _ => {
                        // ignore malformed or unknown lines
                    }
                }
            }
            let _ = child.kill().await;
        });
    }

    // ── build concrete clients ────────────────────────────────────────
    let channel_client = ChannelClient::Rpc(RpcChannelClient::new(rpc_tx.clone(), msg_tx.clone()));
    let control_client = ControlClient::Rpc(RpcControlClient::new(rpc_tx.clone()));

    // assemble the handle
    Ok(PluginHandle::new(channel_client, control_client).await)
}

// -----------------------------------------------------------------------------
// Launch plugin actor and return channels
// -----------------------------------------------------------------------------
pub async fn run_plugin_instance<P>(plugin: Arc<P>) -> PluginHandle
where
    Arc<P>: PluginHandler
        + ChannelClientType // for send / next_inbound
        + HasStore // for config / secret stores
        + Send
        + Sync
        + 'static,
{
    use tokio::sync::{broadcast, mpsc};

    /* ─────────────── 1. plumbing ───────────────────────────────────────── */

    // JSON-RPC path (request / response)
    let (rpc_tx, mut rpc_rx) = mpsc::channel::<(Request, oneshot::Sender<Response>)>(32);

    // broadcast for inbound ChannelMessage events
    let (msg_tx, _msg_rx) = broadcast::channel::<ChannelMessage>(32);

    {
        let mut plugin_clone = plugin.clone();
        tokio::spawn(async move {
            while let Some((req, tx_rsp)) = rpc_rx.recv().await {
                let rsp = handle_internal_request(&mut plugin_clone, req).await;
                let _ = tx_rsp.send(rsp);
            }
        });
    }

    /* ─────────────── 3. message-poller → broadcast  ─────────────────────── */
    {
        let mut plugin_poll = plugin.clone();
        let msg_tx_clone = msg_tx.clone();
        tokio::spawn(async move {
            loop {
                let res = plugin_poll.receive_message().await;
                if res.error || msg_tx_clone.send(res.message).is_err() {
                    break;
                }
            }
        });
    }

    /* ─────────────── 4. concrete clients & handle  ──────────────────────── */
    let channel_client = ChannelClient::new(rpc_tx.clone(), msg_tx.clone());
    let control_client = ControlClient::new(rpc_tx.clone());

    PluginHandle::new(channel_client, control_client).await
}

// -----------------------------------------------------------------------------
// Request handler
// -----------------------------------------------------------------------------

/// Dispatch a single JSON-RPC request **inside** an actor task
/// and return the matching `Response` so the caller can decide
/// what to do with it (e.g. forward to stdout or a channel).
/// Dispatch a JSON-RPC request directly against an *in-process* plugin
/// (any value that implements `PluginHandler`).
pub async fn handle_internal_request<P>(plugin: &mut P, req: Request) -> Response
where
    P: PluginHandler + Send + Sync,
{
    use serde_json::json;

    // convenience builders --------------------------------------------------
    fn ok_null(id: Id) -> Response {
        Response::success(id, json!(null))
    }
    fn err(id: Id, c: i64, m: &str) -> Response {
        Response::fail(id, c, m, None)
    }

    let id = req.id.clone().unwrap_or(Id::Null);

    match req.method.parse::<Method>() {
        /* ───────────── control lifecycle ───────────── */
        Ok(Method::Init | Method::Start) => match req
            .params
            .and_then(|v| serde_json::from_value::<InitParams>(v).ok())
        {
            Some(p) => Response::success(id, json!(plugin.start(p).await)),
            None => err(id, -32602, "Invalid params"),
        },

        Ok(Method::Drain) => Response::success(id, json!(plugin.drain().await)),
        Ok(Method::Stop) => Response::success(id, json!(plugin.stop().await)),

        Ok(Method::WaitUntilDrained) => match req
            .params
            .clone()
            .and_then(|v| serde_json::from_value::<WaitUntilDrainedParams>(v).ok())
        {
            Some(p) => {
                let result = plugin.wait_until_drained(p).await;
                Response::success(id, json!(result))
            }
            None => err(id, -32602, "Invalid params"),
        },

        /* ───────────── informational ──────────────── */
        Ok(Method::State) => {
            let state = plugin.state().await;
            Response::success(id, json!(state))
        }
        Ok(Method::Name) => Response::success(id, json!(plugin.name())),
        Ok(Method::Health) => Response::success(id, json!(plugin.health().await)),
        Ok(Method::Capabilities) => Response::success(id, json!(plugin.capabilities())),
        Ok(Method::ListConfigKeys) => Response::success(id, json!(plugin.list_config_keys())),
        Ok(Method::ListSecretKeys) => Response::success(id, json!(plugin.list_secret_keys())),

        /* ───────────── message I/O ─────────────────── */
        Ok(Method::MessageOut) => match req
            .params
            .and_then(|v| serde_json::from_value::<MessageOutParams>(v).ok())
        {
            Some(p) => Response::success(id, json!(plugin.send_message(p).await)),
            None => err(id, -32602, "Invalid params"),
        },

        Ok(Method::MessageIn) => Response::success(id, json!(plugin.receive_message().await)),

        /* ───────────── config / secrets ───────────── */
        Ok(Method::SetConfig) => match req
            .params
            .and_then(|v| serde_json::from_value::<SetConfigParams>(v).ok())
        {
            Some(p) => {
                plugin.set_config(p).await;
                ok_null(id)
            }
            None => err(id, -32602, "Invalid params"),
        },

        Ok(Method::SetSecrets) => match req
            .params
            .and_then(|v| serde_json::from_value::<SetSecretsParams>(v).ok())
        {
            Some(p) => {
                plugin.set_secrets(p).await;
                ok_null(id)
            }
            None => err(id, -32602, "Invalid params"),
        },

        /* ───────────── unknown method ─────────────── */
        _ => err(id, -32601, "Method not found"),
    }
}

#[cfg(test)]
mod tests {

    use crate::{plugin_runtime::VERSION, plugin_test_util::MockChannel};

    use super::*;

    #[tokio::test]
    async fn test_plugin_handle_capabilities_and_state() {
        let (plugin, _) = PluginHandle::in_process(MockChannel::new()).await.unwrap();

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

        //let _ = plugin.next_message().await.expect("expected messageIn");
    }

    #[tokio::test]
    async fn test_plugin_handle_send_message() {
        let (plugin, _ev_rx) = PluginHandle::in_process(MockChannel::new()).await.unwrap();

        let msg = MessageOutParams {
            message: ChannelMessage {
                from: Participant::new("bot".into(), None, None),
                to: vec![Participant::new("user".into(), None, None)],
                content: vec![MessageContent::Text {
                    text: "hello".into(),
                }],
                ..Default::default()
            },
        };

        let _ = plugin
            .send_message(msg.message)
            .await
            .expect("could not send");
    }

    #[tokio::test]
    async fn test_plugin_handle_start() {
        let (plugin, _ev_rx) = PluginHandle::in_process(MockChannel::new()).await.unwrap();

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
        let mock = MockChannel::new();
        // fresh mock
        let (mut plugin, _ev_rx) = PluginHandle::in_process(mock.clone()).await.unwrap();

        // ActorMockPlugin::receive_message always returns the static "hello"
        let mut msg = ChannelMessage::default();
        msg.content = vec![MessageContent::Text {
            text: "hello".into(),
        }];
        mock.inject(msg).await;
        let msg_in = plugin.next_message().await.expect("RPC failed");

        assert_eq!(
            msg_in.content[..],
            [MessageContent::Text {
                text: "hello".into()
            }],
            "expected the hard-coded text returned by the mock"
        );
    }

    #[tokio::test]
    async fn test_plugin_wait_until_drained() {
        let (plugin, _ev_rx) = PluginHandle::in_process(MockChannel::new()).await.unwrap();

        // fire-and-forget drain
        plugin.drain().await.unwrap();

        // should complete without timing out
        let result = plugin.wait_until_drained(250).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_plugin_list_config_keys_and_secrets() {
        let (plugin, _ev_rx) = PluginHandle::in_process(MockChannel::new()).await.unwrap();

        let cfg = plugin.list_config_keys();
        let sec = plugin.list_secret_keys();

        assert!(cfg.required_keys.is_empty());
        assert!(cfg.optional_keys.is_empty());
        assert!(sec.required_keys.is_empty());
        assert!(sec.optional_keys.is_empty());
    }

    #[tokio::test]
    async fn test_plugin_name_via_rpc() {
        // 1) spawn the mock plugin process
        let handle: PluginHandle =
            spawn_rpc_plugin("../../greentic/plugins/channels/stopped/channel_mock_inout")
                .await
                .expect("failed to start mock plugin");

        // 2) call `name()` through the control client
        let name = handle
            .control_client()
            .name()
            .await
            .expect("RPC name() failed");

        // 3) assert we got the expected identifier
        assert_eq!(name, "mock_inout");
    }
}
