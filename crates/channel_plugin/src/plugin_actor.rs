use std::path::Path;
use std::sync::Arc;
use anyhow::Result;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
#[cfg(feature = "test-utils")]
use serde_json::json;
use strum_macros::{AsRefStr, Display, EnumString};
use tokio::sync::{mpsc, oneshot};
use crate::channel_client::{ChannelClient, CloneableChannelClient, RpcChannelClient};
#[cfg(feature = "test-utils")]
use crate::channel_client::InProcChannelClient;
use crate::control_client::{CloneableControlClient, ControlClient, RpcControlClient};
use crate::jsonrpc::{Message, Request, Response};
#[cfg(feature = "test-utils")]
use crate::jsonrpc::Id;
use crate::message::*;
#[cfg(feature = "test-utils")]
use crate::plugin_runtime::PluginHandler;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command as TokioCommand,
    sync::{broadcast},
};
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
pub type PluginClient = PluginHandle<dyn ChannelClient, dyn ControlClient>;
pub type CloneablePluginClient = PluginHandle<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>;
/// Returned when you spawn a plugin in-process.
/// Use it to send JSON-RPC requests to the plugin.
#[derive( Debug)]
pub struct PluginHandle<C, R>
where
    C: ChannelClient + Clone + Send + Sync + 'static,
    R: ControlClient + Clone + Send + Sync + 'static,
{
    /// “Slow-path” control channel (unchanged)
    cmd_tx: mpsc::Sender<Command>,

    /// Message in/out abstraction – could be JSON-RPC, a gRPC stub,
    /// a future NATS client, … whatever implements `ChannelClient`.
    ///
    /// Notice we *clone* the client for every handle to keep
    /// `PluginHandle` itself `Clone`.
    client: C,
    control: R,
    plugin_id: String,
}

impl<C,R> Clone for PluginHandle<C,R>
where
    C: ChannelClient + Clone + Send + Sync + 'static,
    R: ControlClient + Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            cmd_tx: self.cmd_tx.clone(),
            client: self.client.clone(),
            control: self.control.clone(),
            plugin_id: self.plugin_id.clone(),
        }
    }
}


impl<C, R> PluginHandle<C, R>
where
    C: ChannelClient + Clone + Send + Sync + 'static,
    R: ControlClient + Clone + Send + Sync + 'static,
{
    pub fn new(cmd_tx: mpsc::Sender<Command>, client: C, control: R, plugin_id: String) -> Self {
        Self {
            cmd_tx,
            client,
            control,
            plugin_id,
        }
    }
    
    pub fn channel_client(&self) -> Box<dyn CloneableChannelClient>{
        Box::new(self.client.clone())
    }
    
    pub fn control_client(&self) -> Box<dyn CloneableControlClient>{
       Box::new(self.control.clone())
    }

    pub fn id(&self) -> &str {
        &self.plugin_id
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


    pub async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
        self.control.capabilities().await
    }

    pub async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult> {
        self.control.list_config_keys().await
    }

    pub async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult> {
        self.control.list_secret_keys().await
    }

    pub async fn start(&self, params: InitParams) -> anyhow::Result<InitResult> {
        self.control.start(params).await
    }



    pub async fn drain(&self) -> anyhow::Result<()> {
        self.control.drain().await
    }

    pub async fn stop(&self) -> anyhow::Result<()> {
        self.control.stop().await
    }

    pub async fn health(&self) -> anyhow::Result<HealthResult> {
        self.control.health().await
    }

    pub async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<()> {
        self.control.wait_until_drained(timeout_ms).await
    }


     /* ────────────────────────────────────────────────────────────────────────
     * 1)  In-process actor
     * ──────────────────────────────────────────────────────────────────────── */
    #[cfg(feature = "test-utils")]
    pub async fn in_process<P>(
        plugin: P,
    ) -> anyhow::Result<(PluginHandle<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>,
                        mpsc::UnboundedReceiver<(String, Request)>)>
    where
        P: PluginHandler + Clone + 'static,
    {
        // ① run the actor → gives us a fully-wired handle
        let handle: PluginHandle<Box<dyn CloneableChannelClient>,Box<dyn CloneableControlClient>> =
            run_plugin_instance(plugin).await;

        // ② dummy “events” receiver (keeps the old tests compiling)
        let (_ev_tx, ev_rx) = mpsc::unbounded_channel::<(String, Request)>();

        Ok((handle, ev_rx))
    }

}


pub async fn spawn_rpc_plugin<P>(
    exe: P,
) -> Result<
    PluginHandle<
        Box<dyn CloneableChannelClient>,
        Box<dyn CloneableControlClient>,
    >
>
where
    P: AsRef<Path>,
{
    // ── launch the child process ──────────────────────────────────────
    let mut child = TokioCommand::new(exe.as_ref())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .expect("failed to open plugin stdin");
    let stdout = child
        .stdout
        .take()
        .expect("failed to open plugin stdout");

    // ── JSON-RPC actor queue ──────────────────────────────────────────
    let (rpc_tx, mut rpc_rx) =
        mpsc::channel::<(Request, oneshot::Sender<Response>)>(32);

    // broadcast for inbound ChannelMessage events (`messageIn`)
    let (msg_tx, _) = broadcast::channel::<ChannelMessage>(32);

    // map of <id → responder> for inflight calls
    let inflight: Arc<DashMap<String, oneshot::Sender<Response>>> =
        Arc::new(DashMap::new());

    // ── task: forward (Request, rsp_tx) → child.stdin ─────────────────
    {
        let inflight = Arc::clone(&inflight);
        tokio::spawn(async move {
            while let Some((req, rsp_tx)) = rpc_rx.recv().await {
                // remember the responder
                if let Some(id) = &req.id {
                    inflight.insert(
                        serde_json::to_string(id).unwrap(),
                        rsp_tx,
                    );
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
                    Ok(Message::Request(req))
                        if req.method == "messageIn" =>
                    {
                        if let Some(p) = req.params {
                            if let Ok(r) =
                                serde_json::from_value::<MessageInResult>(p)
                            {
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
    let channel_client =
        RpcChannelClient::new(rpc_tx.clone(), msg_tx.clone());
    let control_client = RpcControlClient::new(rpc_tx.clone());

    // dummy cmd channel (unused for RPC plugins but required by struct)
    let (cmd_tx, _cmd_rx) = mpsc::channel::<Command>(1);

    // assemble the handle
    Ok(PluginHandle::new(
        cmd_tx,
        Box::new(channel_client) as Box<dyn CloneableChannelClient>,
        Box::new(control_client) as Box<dyn CloneableControlClient>,
        exe.as_ref()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    ))
}

// -----------------------------------------------------------------------------
// Launch plugin actor and return channels
// -----------------------------------------------------------------------------
#[cfg(feature = "test-utils")]
pub async fn run_plugin_instance<P>(
    plugin: P,
) -> PluginHandle<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>
where
    P: PluginHandler,
{
    use std::sync::Arc;

    use tokio::sync::broadcast;

    use crate::control_client::InProcControlClient;


    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
    let (msg_tx,  msg_rx)   = broadcast::channel::<ChannelMessage>(32);
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
    let msg_tx_clone = msg_tx.clone();
    tokio::spawn(async move {
        loop {
                let result = plugin_poll.receive_message().await;
                println!("@@@ REMOVE receive");
                if result.error {
                    use tracing::error;

                    error!("Could not receive message due to error: {:?}",result.error);
                    break; // break loop if plugin indicates error/closed
                }
                let send_res = msg_tx_clone.send(result.message);
                if send_res.is_err() { break; }  // receiver dropped
            }
    });
    let client = Box::new(InProcChannelClient::new(
        cmd_tx.clone(),
        msg_rx,
        Arc::new(msg_tx),
    ));

    let control = Box::new(InProcControlClient::new(
        cmd_tx.clone()
    ));

    PluginHandle::new(cmd_tx, client, control,name)
}




// -----------------------------------------------------------------------------
// Request handler
// -----------------------------------------------------------------------------

/// Dispatch a single JSON-RPC request **inside** an actor task
/// and return the matching `Response` so the caller can decide
/// what to do with it (e.g. forward to stdout or a channel).
#[cfg(feature = "test-utils")]
pub async fn handle_internal_request<P: PluginHandler>(
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
    use std::sync::Arc;

    use crate::{plugin_runtime::VERSION, plugin_test_util::MockChannel};

    use super::*;
    
    #[tokio::test]
    async fn test_plugin_handle_capabilities_and_state() {
        let (plugin, _) = PluginHandle::<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>::in_process(Arc::new(MockChannel::new())).await.unwrap();

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
        let (plugin, _ev_rx) = PluginHandle::<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>::in_process(Arc::new(MockChannel::new())).await.unwrap();

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
        let (plugin, _ev_rx) = PluginHandle::<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>::in_process(Arc::new(MockChannel::new())).await.unwrap();

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
        let mock = Arc::new(MockChannel::new());
        // fresh mock
        let (mut plugin, _ev_rx) =
            PluginHandle::<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>::in_process(Arc::new(MockChannel::new())).await.unwrap();

        // ActorMockPlugin::receive_message always returns the static "hello"
        let mut msg = ChannelMessage::default();
        msg.content = vec![MessageContent::Text{text: "hello".into() }];
        mock.inject(msg).await;
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
            PluginHandle::<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>::in_process(Arc::new(MockChannel::new())).await.unwrap();

        // fire-and-forget drain
        plugin.drain().await.unwrap();

        // should complete without timing out
        plugin.wait_until_drained(250).await.unwrap();
    }

    #[tokio::test]
    async fn test_plugin_list_config_keys_and_secrets() {
        let (plugin, _ev_rx) =
            PluginHandle::<Box<dyn CloneableChannelClient>, Box<dyn CloneableControlClient>>::in_process(Arc::new(MockChannel::new())).await.unwrap();

        let cfg = plugin.list_config_keys().await.unwrap();
        let sec = plugin.list_secret_keys().await.unwrap();

        assert!(cfg.required_keys.is_empty());
        assert!(cfg.optional_keys.is_empty());
        assert!(sec.required_keys.is_empty());
        assert!(sec.optional_keys.is_empty());
    }
}
