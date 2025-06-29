use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use dashmap::DashMap;
use serde::de::DeserializeOwned;
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


    pub async fn from_executable<P: AsRef<Path>>(
        exe_path: P,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<PluginEvent>)> {
        // 1. We create a fresh channel for the plugin-initiated *events*
        let (event_tx, event_rx) = mpsc::unbounded_channel::<PluginEvent>();

        // 2. Re-use your existing helper that already knows how to
        //    spawn the child-process and wire stdin/stdout.
        let handle = spawn_plugin(exe_path, event_tx).await?;

        // 3. Return both pieces to the caller.
        Ok((handle, event_rx))
    }

    /// Convenience variant if you do **not** care about the event stream.
    ///
    /// Equivalent to `from_executable(...).await.map(|(h, _)| h)`
    pub async fn from_executable_no_events<P: AsRef<Path>>(
        exe_path: P,
    ) -> anyhow::Result<Self> {
        let (handle, _event_rx) = Self::from_executable(exe_path).await?;
        Ok(handle)
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

// -----------------------------------------------------------------------------
// Runtime main loop
// -----------------------------------------------------------------------------

pub async fn run<P: PluginHandler>(plugin: P) -> PluginHandle {
    // ── a) start the plugin as an actor  ────────────────────────────────
    //
    //     spawn_plugin_actor returns:
    //       * cmd_tx  —  Sender<(Request, oneshot::Sender<Response>)>
    //       * event_rx — Receiver<MessageInResult>        (or whatever type you emit)
    //
    let (cmd_tx, mut event_rx) = spawn_plugin_actor(plugin.clone()).await;

    // ── b) build the public ActorHandle  ────────────────────────────────
    //
    //     We put *another* small mpsc queue in front so that external
    //     callers don’t talk to the actor’s cmd_tx directly (nice for
    //     back-pressure & decoupling).
    //
    let (tx, mut rx) = mpsc::channel::<(Request, oneshot::Sender<Response>)>(32);
    let plugin_id    = plugin.name().name.clone();  

    // forward our public queue → real actor queue
    tokio::spawn({
        let cmd_tx = cmd_tx.clone();
        async move {
            while let Some((req, rsp_tx)) = rx.recv().await {
                // just proxy into the real actor
                if cmd_tx.send(Command::Call(req, rsp_tx)).await.is_err() {
                    break; // actor died
                }
            }
        }
    });

    // ── c) reader task: stdin → plugin  ─────────────────────────────────
    let cmd_tx_stdin = cmd_tx.clone();
    tokio::spawn(async move {
        let mut rdr = BufReader::new(tokio::io::stdin()).lines();

        while let Ok(Some(line)) = rdr.next_line().await {
            if line.trim().is_empty() { continue; }

            if let Ok(Message::Request(req)) = serde_json::from_str::<Message>(&line) {
                // fire-and-forget if it’s a notification
                // or await a response if there’s an id
                if req.id.is_none() {
                    // notification → no response channel needed
                    let (dummy_tx, _dummy_rx) = oneshot::channel();
                    let _ = cmd_tx_stdin.send(Command::Call(req, dummy_tx)).await;
                } else {
                    // request → we must send back a Response
                    let (tx_resp, rx_resp) = oneshot::channel();
                    if cmd_tx_stdin.send(Command::Call(req, tx_resp)).await.is_err() {
                        break;
                    }
                    // write the response to stdout
                    if let Ok(resp) = rx_resp.await {
                        let s = format!("{}\n", serde_json::to_string(&resp).unwrap());
                        let _ = tokio::io::stdout().write_all(s.as_bytes()).await;
                    }
                }
            }
        }
    });

    // ── d) writer task: plugin events → stdout  ─────────────────────────
    tokio::spawn(async move {
        while let Some(msg) = event_rx.recv().await {
            let notif = Request::notification("messageIn", Some(json!(msg)));
            let line  = format!("{}\n", serde_json::to_string(&notif).unwrap());
            let _ = tokio::io::stdout().write_all(line.as_bytes()).await;
        }
    });

    // ── e) return handle ────────────────────────────────────────────────
    PluginHandle { tx, plugin_id }
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
