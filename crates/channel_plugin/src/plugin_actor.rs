use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use dashmap::DashMap;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::{channel, unbounded_channel};
use tokio::sync::{mpsc, oneshot};
use anyhow::{Result, anyhow};
use serde_json::{json, Value};
use crate::jsonrpc::{Id, Message, Request, Response};
use crate::message::*;
use crate::plugin_runtime::PluginHandler;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use uuid::Uuid;
// -----------------------------------------------------------------------------
// Define Commands for actor
// -----------------------------------------------------------------------------

pub enum Command {
    Init(InitParams, oneshot::Sender<InitResult>),
    Drain,
    Stop,
    State(oneshot::Sender<StateResult>),
    Health(oneshot::Sender<HealthResult>),
    SendMessage(MessageOutParams, oneshot::Sender<MessageOutResult>),
    SetConfig(SetConfigParams),
    SetSecrets(SetSecretsParams),
    Call(Request, oneshot::Sender<Response>),
}


// Event sent from plugin to outside
pub type PluginEvent = (String, Request);

/// Returned when you spawn a plugin in-process.
/// Use it to send JSON-RPC requests to the plugin.
#[derive(Clone, Debug)]
pub struct PluginHandle {
    tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
    plugin_id: String,
}

impl PluginHandle {
    pub fn new(tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,plugin_id: String,) -> Self {
        Self{tx,plugin_id}
    }
    pub async fn call(&self, req: Request) -> Result<Response> {
        let (tx_resp, rx_resp) = oneshot::channel();
        self.tx.send((req, tx_resp)).await
            .map_err(|_| anyhow!("Plugin actor '{}' is dead", self.plugin_id))?;
        rx_resp.await.map_err(|_| anyhow!("Plugin actor '{}' dropped response", self.plugin_id))
    }

    pub fn id(&self) -> &str {
        &self.plugin_id
    }

    /// Small generic helper: call any JSON-RPC method and deserialize the
    /// `.result` into the requested type.
    async fn rpc_call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<T> {
        // 1. build request
        let req = Request::call(
            Id::String(Uuid::new_v4().to_string()),
            method,
            params,
        );

        // 2. round-trip via existing `call`
        let rsp = self.call(req).await?;

        // 3. convert the generic `Value` into concrete type `T`
        let v = rsp.result.ok_or_else(|| anyhow!("no result field"))?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn rpc_notify<P: serde::Serialize>(
        &self,
        method: &str,
        params: Option<P>,
    ) -> anyhow::Result<()> {
        let req = Request {
            jsonrpc: "2.0".to_string(),
            id: None, // <-- no response expected
            method: method.to_string(),
            params: params.map(|p| serde_json::to_value(p)).transpose()?,
        };

        // Just fire and forget — use a dummy oneshot::Sender
        let (_tx, _rx) = tokio::sync::oneshot::channel();
        self.tx.send((req, _tx)).await
            .map_err(|_| anyhow::anyhow!("ActorHandle '{}' is dead", self.plugin_id))?;

        Ok(())
    }

    // ---------------------------------------------------------------------
    // Convenience wrappers
    // ---------------------------------------------------------------------

    /// `capabilities` → `CapabilitiesResult`
    pub async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
        match self.rpc_call::<CapabilitiesResult>("capabilities", None).await {
            Ok(cap) => Ok(cap.capabilities),
            Err(err) => Err(err),
        }
    }

    /// `listConfigKeys` → Vec<String>
    pub async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult> {
        self.rpc_call::<ListKeysResult>("listConfigKeys", None).await
    }

    /// `listSecretKeys` → Vec<String>
    pub async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult> {
       self.rpc_call::<ListKeysResult>("listSecretKeys", None).await
    }

    pub async fn start(&mut self, params: InitParams) ->  anyhow::Result<InitResult>{
        let msg = serde_json::to_value(params)
        .map_err(|err| anyhow!("Failed to serialize InitParams: {}", err))?;
        self.rpc_call::<InitResult>("start", Some(msg)).await

    }

    pub async fn send_message(&mut self, params: MessageOutParams) ->  anyhow::Result<MessageOutResult>{
        let msg = serde_json::to_value(params)
        .map_err(|err| anyhow!("Failed to serialize MessageOutParams: {}", err))?;
        self.rpc_call::<MessageOutResult>("sendMessage", Some(msg)).await

    }
    /// When a message comes in
    pub async fn receive_message(&mut self) ->  anyhow::Result<MessageInResult>{
       self.rpc_call::<MessageInResult>("receiveMessage", None).await
    }

    /// `state` → `StateResult`
    pub async fn state(&self) -> anyhow::Result<ChannelState> {
        match self.rpc_call::<StateResult>("state", None).await{
            Ok(state) => Ok(state.state),
            Err(err) => Err(err),
        }
    }

    pub async fn drain(&self)  -> anyhow::Result<()> {
       self.rpc_notify::<()>("drain", None).await
    }

    pub async fn stop(&self)  -> anyhow::Result<()> {
       self.rpc_notify::<()>("drain", None).await
    }

    /// `health` → `HealthResult`
    pub async fn health(&self) -> anyhow::Result<HealthResult> {
        self.rpc_call::<HealthResult>("health", None).await
    }

    /// Example with params: `waitUntilDrained`
    pub async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<()> {
        #[derive(serde::Serialize)]
        struct DrainParams { timeout_ms: u64 }

        // If the method returns just `null`, ask for `Value`
        let _: Value = self
            .rpc_call("waitUntilDrained", Some(serde_json::to_value(DrainParams { timeout_ms })?))
            .await?;
        Ok(())
    }


     /* ────────────────────────────────────────────────────────────────────────
     * 1)  In-process actor
     * ──────────────────────────────────────────────────────────────────────── */
    pub async fn in_process<P: PluginHandler>(
        plugin: P,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<PluginEvent>)> {

        // (a) spawn the plugin actor
        let (cmd_tx, mut msg_rx) = spawn_plugin_actor(plugin).await;

        // (b) create JSON-RPC channel for external requests
        let (tx, mut rx) = channel::<(Request, oneshot::Sender<Response>)>(32);

        // (c) forward requests → actor commands
        let cmd_tx_clone = cmd_tx.clone();
        tokio::spawn(async move {
            while let Some((req, rsp_tx)) = rx.recv().await {
                let _ = cmd_tx_clone.send(Command::Call(req, rsp_tx)).await;
            }
        });

        // (d) translate MessageInResult → PluginEvent
        let (ev_tx, ev_rx) = unbounded_channel::<PluginEvent>();
        let plugin_id = "in-process".to_string();
        let plugin_id_clone = plugin_id.clone();
        tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                let req = Request::notification("messageIn", Some(json!(msg)));
                let _ = ev_tx.send((plugin_id_clone.clone(), req));
            }
        });

        Ok((Self::new(tx, plugin_id), ev_rx))
    }

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
}

// -----------------------------------------------------------------------------
// Launch plugin actor and return channels
// -----------------------------------------------------------------------------

pub async fn spawn_plugin_actor<P: PluginHandler>(mut plugin: P)
    -> (mpsc::Sender<Command>, mpsc::Receiver<MessageInResult>)
{
    let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
    let (event_tx, event_rx) = mpsc::channel(16);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => match cmd {
                    Command::Init(p, tx) => {
                        let _ = tx.send(plugin.start(p).await);
                    }
                    Command::Drain => {
                        plugin.drain().await;
                    }
                    Command::Stop => {
                        plugin.stop().await;
                    }
                    Command::State(tx) => {
                        let _ = tx.send(plugin.state().await);
                    }
                    Command::Health(tx) => {
                        let _ = tx.send(plugin.health().await);
                    }
                    Command::SendMessage(p, tx) => {
                        let _ = tx.send(plugin.send_message(p).await);
                    }
                    Command::SetConfig(p) => {
                        let _ = plugin.set_config(p).await;
                    }
                    Command::SetSecrets(p) => {
                        let _ = plugin.set_secrets(p).await;
                    },
                    Command::Call(req, tx) => {
                        let response = handle_internal_request(&mut plugin, req).await;
                        let _ = tx.send(response);
                    }
                },

                result = plugin.receive_message() => {
                    let _ = event_tx.send(result).await;
                }
            }
        }
    });

    (cmd_tx, event_rx)
}




/// Launch `exe_path` as a child-process, wire JSON-RPC over stdin/stdout
/// and return an `ActorHandle` that speaks plain JSON-RPC.
///
/// `event_tx` is forwarded every time the plugin sends a *request*
/// (e.g. `messageIn`).  
pub async fn spawn_plugin<P: AsRef<Path>>(
    exe_path: P,
    event_tx: mpsc::UnboundedSender<PluginEvent>,
) -> anyhow::Result<PluginHandle> {
    // ── launch ───────────────────────────────────────────────────────
    let mut child = TokioCommand::new(exe_path.as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin  = child.stdin.take().expect("stdin unavailable");
    let  stdout    = child.stdout.take().expect("stdout unavailable");

    // ── actor queue (JSON-RPC <Request, Response>) ───────────────────
    let (tx, mut rx) = mpsc::channel::<(Request, oneshot::Sender<Response>)>(32);

    // track in-flight calls by encoded `id`
    let inflight: Arc<DashMap<String, oneshot::Sender<Response>>> = Arc::new(DashMap::new());
    {
        let inflight = Arc::clone(&inflight);
        // ── task that proxies rx → child.stdin ───────────────────────────
        tokio::spawn(async move {
            while let Some((req, rsp_tx)) = rx.recv().await {
                // remember responder
                if let Some(id) = &req.id {
                    inflight.insert(serde_json::to_string(id).unwrap(), rsp_tx);
                }
                // write line
                if stdin.write_all(serde_json::to_string(&req).unwrap().as_bytes()).await.is_err() { break }
                let _ = stdin.write_all(b"\n").await;
                let _ = stdin.flush().await;        // ignore error → handled next read
            }
        });
    }

    // ── task that reads child.stdout → routes Response|Request ───────
    let plugin_id = exe_path.as_ref()
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("plugin")
                            .to_string();
    let plugin_id_clone = plugin_id.clone();
    {
        let inflight = Arc::clone(&inflight);
        tokio::spawn(async move {
            let mut rdr = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = rdr.next_line().await {
                if line.trim().is_empty() { continue; }
                match serde_json::from_str::<Message>(&line) {
                    Ok(Message::Response(rsp)) => {
                        let key = serde_json::to_string(&rsp.id).unwrap();
                        if let Some((_, tx_rsp)) = inflight.remove(&key) { let _ = tx_rsp.send(rsp); }
                    }
                    Ok(Message::Request(req)) => {
                        let _ = event_tx.send((plugin_id_clone.clone(), req));
                    }
                    _ => {}   // bad line ⇒ ignore
                }
            }
            let _ = child.kill().await;
        });
    }

    Ok( PluginHandle { tx, plugin_id } )

}
// -----------------------------------------------------------------------------
// Request handler
// -----------------------------------------------------------------------------

/// Dispatch a single JSON-RPC request **inside** an actor task
/// and return the matching `Response` so the caller can decide
/// what to do with it (e.g. forward to stdout or a channel).
async fn handle_internal_request<P: PluginHandler>(
    plugin: &mut P,
    req: Request,
) -> Response {
    // Convenience helpers ----------------------------------------------------
    fn ok_null(id: Id) -> Response {
        Response::success(id, json!(null))
    }

    fn err(id: Id, code: i64, msg: &str, data: Option<Value>) -> Response {
        Response::fail(id, code, msg, data)
    }

    // ------------------------------------------------------------------------
    match req.method.as_str() {
        // ─────────────── notifications / calls the runner understands ───────
        "init" => {
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

        "drain" => {
            let id = req.id.unwrap_or(Id::Null);
            plugin.drain().await;
            ok_null(id)
        }

        "messageOut" => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<MessageOutParams>(v).ok())
            {
                Some(p) => Response::success(id, json!(plugin.send_message(p).await)),
                None    => err(id, -32602, "Invalid params", None),
            }
        }

        "health" => {
            let id = req.id.unwrap_or(Id::Null);
            Response::success(id, json!(plugin.health().await))
        }

        "state" => {
            let id = req.id.unwrap_or(Id::Null);
            Response::success(id, json!(plugin.state().await))
        }

        "setConfig" => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<SetConfigParams>(v).ok())
            {
                Some(p) => Response::success(id, json!(plugin.set_config(p).await)),
                None    => err(id, -32602, "Invalid params", None),
            }
        }

        "setSecrets" => {
            let id = req.id.unwrap_or(Id::Null);
            match req.params
                    .and_then(|v| serde_json::from_value::<SetSecretsParams>(v).ok())
            {
                Some(p) => Response::success(id, json!(plugin.set_secrets(p).await)),
                None    => err(id, -32602, "Invalid params", None),
            }
        }

        // ─────────────── unknown / unsupported method ───────────────────────
        _ => {
            let id = req.id.unwrap_or(Id::Null);
            err(id, -32601, "Method not found", None)
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod tests {
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


}