//! Async runtime that wires **stdin / stdout** JSON‑RPC traffic to a user‑supplied
//! `PluginHandler` implementation.
//!
//! ### Key design goals
//! * **Zero dependencies** beyond `tokio`, `serde_json`, `async‑trait`
//! * Works with the existing `jsonrpc` and `message` modules we defined earlier
//! * Handles:
//!   * Requests → method dispatch → JSON‑RPC response
//!   * Notifications (no `id`) → fire‑and‑forget
//!   * Basic error handling (invalid JSON‑RPC, unknown method, panics)
//!
//! Usage:
//! ```ignore
//! use channel_plugin::PluginRuntime;
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let my_plugin = MyPlugin::default();
//!     let mut runtime = PluginRuntime::new(my_plugin);
//!     runtime.run().await;
//! }
//! ```

use std::{panic, time::Instant};

use async_trait::async_trait;
use anyhow::Result;
use dashmap::DashMap;
use tracing::{dispatcher, level_filters::LevelFilter, Dispatch};
use tracing_appender::rolling::daily;
use tracing_subscriber::{fmt, Registry};
use crate::{jsonrpc::{Id, Message, Request, Response}, plugin_actor::Method};
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::Layer;
use serde_json::{json, Value};
use tokio::{io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter}, sync::{mpsc::{self, UnboundedSender}}, time::sleep};
use crate::message::*;


// -----------------------------------------------------------------------------
// PluginHandler trait – implement this in your plugin code
// -----------------------------------------------------------------------------

///  A tiny trait that just gives access to the store.
///
///  Every plugin struct that wants to use the default `set_config` /
///  `set_secrets` impls only has to return a reference to its map.
pub trait HasStore {
    fn config_store(&self) -> &DashMap<String, String>;
    fn secret_store(&self) -> &DashMap<String, String>;
}

pub const VERSION: &str = "0.1.0";

#[async_trait]
pub trait PluginHandler: HasStore + Send + Sync + Clone + 'static {
    /// Initialise the plugin and start any underlying services
    async fn init(&mut self, params: InitParams) -> InitResult;
    async fn start(&mut self, params: InitParams) -> InitResult {
        let res = self.init_from_params(&params).await;
        if !res.success {
            return res;
        }
        self.init(params).await
    }
    /// Drain the plugin
    async fn drain(&mut self);
    async fn wait_until_drained(&self, params: WaitUntilDrainedParams) -> WaitUntilDrainedResult {
        let deadline = Instant::now() + std::time::Duration::from_millis(params.timeout_ms);

        loop {
            let state = self.state().await.state;

            if state == ChannelState::STOPPED {
                return WaitUntilDrainedResult{stopped: true, error: false};
            }

            if Instant::now() >= deadline {
                return WaitUntilDrainedResult{stopped: false, error: true};
            }

            sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    /// Stop the plugin
    async fn stop(&mut self);
    /// When a message needs to be send
    async fn send_message(&mut self, params: MessageOutParams) -> MessageOutResult;
    /// When a message comes in
    async fn receive_message(&mut self) -> MessageInResult;
    /// Check the health of the plugin
    async fn health(&self) -> HealthResult {
        HealthResult { healthy: true, reason: None }
    }
    async fn version(&self) -> VersionResult{
        VersionResult{version: VERSION.to_string()}
    }
    /// Request the current status
    async fn state(&self) -> StateResult;
    /// Set the configuration
    async fn set_config(&mut self, p: SetConfigParams) -> SetConfigResult {
        // 1. persist the keys we just received
        for (k, v) in p.config {
            self.config_store().insert(k, v);
        }
        // 2. gather the list of required keys
        let required: std::collections::HashSet<_> =
            self.list_config_keys().required_keys.into_iter().map(|(k, _)| k).collect();

         // 3. check which of them are still missing
        let missing: Vec<_> = required
            .into_iter()
            .filter(|k| !self.config_store().contains_key(k))
            .collect();

        if missing.is_empty() {
            SetConfigResult { success: true, error: None }
        } else {
            let msg = format!("missing required config keys: {}", missing.join(", "));
            SetConfigResult { success: false, error: Some(msg) }
        }
    }
    /// Set the secrets
    async fn set_secrets(&mut self, p: SetSecretsParams) -> SetSecretsResult {
        for (k, v) in p.secrets {
            self.secret_store().insert(k, v);
        }
        let required: std::collections::HashSet<_> =
        self.list_secret_keys().required_keys.into_iter().map(|(k, _)| k).collect();

        let missing: Vec<_> = required
            .into_iter()
            .filter(|k| !self.secret_store().contains_key(k))
            .collect();

        if missing.is_empty() {
            SetSecretsResult { success: true, error: None }
        } else {
            let msg = format!("missing required secret keys: {}", missing.join(", "));
            SetSecretsResult { success: false, error: Some(msg) }
        }
    }

    /// Gets a config value from the config store
    fn get_config(&self, key: &str) -> Option<String> {
        self.config_store()            // &DashMap<String, String>
        .get(key)                  // Option< Ref<'_, String, String> >
        .map(|guard| guard.value().clone())
    }
    /// Gets a secret from the secret store
    fn get_secret(&self, key: &str) -> Option<String> {
        self.secret_store()            // &DashMap<String, String>
        .get(key)                  // Option< Ref<'_, String, String> >
        .map(|guard| guard.value().clone())
    }
    /// Returns the plugin name, e.g., "telegram", "ws", etc.
    fn name(&self) -> NameResult;
    /// List of expected config keys (like `API_KEY`, `WS_PORT`, etc.)
    fn list_config_keys(&self) -> ListKeysResult;
    /// List of expected secret keys
    fn list_secret_keys(&self) -> ListKeysResult;
    /// Declares plugin capabilities (sending, receiving, text, etc.)
    fn capabilities(&self) -> CapabilitiesResult;

    /// initialise logging, config and secrets so plugins don't have to
    async fn init_from_params(&mut self, params: &InitParams) -> InitResult{
        static LOG_INIT: std::sync::Once = std::sync::Once::new();
        LOG_INIT.call_once(|| {
            let result = panic::catch_unwind(|| {
                // ── level ───────────────────────────────────────────────
                let level = match params.log_level {
                    LogLevel::Trace    => LevelFilter::TRACE,
                    LogLevel::Debug    => LevelFilter::DEBUG,
                    LogLevel::Info     => LevelFilter::INFO,
                    LogLevel::Warn     => LevelFilter::WARN,
                    LogLevel::Error    => LevelFilter::ERROR,
                    LogLevel::Critical => LevelFilter::ERROR,
                };

                // ── optional file layer  ────────────────────────────────
                let subscriber_dispatch: Dispatch = if let Some(dir) = &params.log_dir {
                    std::fs::create_dir_all(dir).ok();               // ignore error, best-effort
                    let file_app = daily(dir, "plugin.log");

                    Dispatch::new(
                        Registry::default()
                            .with(
                                fmt::layer()
                                    .with_ansi(false)
                                    .with_target(false)
                                    .with_writer(file_app)
                                    .with_filter(level),
                            ),
                    )
                } else {
                    panic!("❌ Logging requires a `log_dir`. None was provided.");
                };

                // ── install ─────────────────────────────────────────────
                dispatcher::set_global_default(subscriber_dispatch)
                    .expect("failed to install tracing subscriber");

                if cfg!(debug_assertions) && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                    tracing::warn!(
                        "‼️  A tracing layer is writing to STDOUT – \
                        this WILL break the JSON-RPC protocol."
                    );
                }
            });
            if result.is_err() {
                eprintln!("❌ Logging setup failed");
            }
        });

        let res = self
            .set_config(SetConfigParams { config: params.config.clone() })
            .await;
        if !res.success {
            return InitResult{ success: false, error: res.error }
        }

        let res = self.set_secrets(SetSecretsParams{secrets:params.secrets.clone()}).await;
        if !res.success {
            return InitResult{ success: false, error: res.error }
        }

        InitResult{ success: true, error: None }
    }
}


// -----------------------------------------------------------------------------
// Runtime function – spawn read / write loops
// -----------------------------------------------------------------------------

/// Runs the JSON‑RPC stdin/stdout loop until EOF or fatal error.
pub async fn run<P: PluginHandler>(mut plugin: P) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut w = BufWriter::new(io::stdout());
        while let Some(line) = rx.recv().await {
            if let Err(e) = w.write_all(line.as_bytes()).await {
                eprintln!("stdout write error: {e}");
                break;              // abort writer task → plugin will exit
            }
            // avoid tight loop when channel is empty
            if w.flush().await.is_err() {
                eprintln!("stdout flush error");
                break;
            }
        }
    });

    // ── 2. spawn poller that turns `receive_message()` into `messageIn` notif
    let poller_tx = tx.clone();
    let mut plugin_clone = plugin.clone();

    tokio::spawn(async move {
        loop {
            let result = plugin_clone.receive_message().await;

            let notif = Request::notification(
                "messageIn",
                Some(serde_json::to_value(result).unwrap()),
            );
            let _ = poller_tx.send(format!("{}\n", serde_json::to_string(&notif).unwrap()));
        }
    });
    // ── 3. read stdin, dispatch requests, send responses via the same tx ─────
    let mut reader = BufReader::new(io::stdin());
    let mut line   = String::new();

    while reader.read_line(&mut line).await? != 0 {
        trim_newlines(&mut line);
        if line.is_empty() { continue; }

        match serde_json::from_str::<Message>(&line) {
            Ok(Message::Request(req)) => {
                handle_request(&mut plugin, req, &tx.clone()).await
            }
            Ok(_) => { /* ignore stray Response/Notif from stdin */ }
            Err(e) => {
                let err = Response::fail(Id::Null, -32700, "Parse error", Some(json!(e.to_string())));
                let _  = tx.send(format!("{}\n", serde_json::to_string(&err).unwrap()));
            }
        }
        line.clear();
    }

    Ok(())
}

fn trim_newlines(s: &mut String) {
    while matches!(s.chars().last(), Some('\n' | '\r')) { s.pop(); }
}

async fn handle_request<P>(
    plugin: &mut P,
    req: Request,
    tx: &UnboundedSender<String>,
) 
where
    P: PluginHandler,
{
    /// Helper that serialises a `Response` and sends it to the writer queue.
    fn enqueue(tx: &UnboundedSender<String>, resp: Response) {
        let _ = tx.send(format!("{}\n", serde_json::to_string(&resp).unwrap()));
    }

    /// Helper that sends `{"result":null}`.
    macro_rules! ok_null {
        ($id:expr) => {
            enqueue(tx, Response::success($id, json!(null)));
        };
    }

    match req.method.parse::<Method>() {
        Ok(Method::Init) => {
            if let Some(v) = req.params {
                if let Ok(p) = serde_json::from_value::<InitParams>(v) { plugin.start(p).await; }
            }
            if let Some(id) = req.id { ok_null!(id); }
        }
        Ok(Method::Drain) => {
            plugin.drain().await;
            if let Some(id) = req.id { ok_null!(id); }
        }
        Ok(Method::MessageOut) => {
            match serde_json::from_value::<MessageOutParams>(req.params.unwrap_or(Value::Null)) {
                Ok(p) => {
                    let result = plugin.send_message(p).await;
                    if let Some(id) = req.id {
                        enqueue(tx, Response::success(id, json!(result)));
                    }
                }
                Err(e) => {
                    if let Some(id) = req.id {
                        enqueue(tx,
                            Response::fail(id, -32602, "Invalid params", Some(json!(e.to_string())))
                        );
                    }
                }
            }
        }
        Ok(Method::Name) => {
            if let Some(id) = req.id {
               enqueue(tx, Response::success(id, json!(plugin.name())));
            }
        }
        Ok(Method::Health) => {
            if let Some(id) = req.id {
               enqueue(tx, Response::success(id, json!(plugin.health().await)));
            }
        }
        Ok(Method::State) => {
            if let Some(id) = req.id {
                enqueue(tx, Response::success(id, json!(plugin.state().await)));
            }
        }
        Ok(Method::Capabilities) => {
            if let Some(id) = req.id {
                enqueue(tx, Response::success(id, json!(plugin.capabilities())));
            }
        }
        Ok(Method::ListConfigKeys) => {
            if let Some(id) = req.id {
                enqueue(tx, Response::success(id, json!(plugin.list_config_keys())));
            }
        }
        Ok(Method::ListSecretKeys) => {
            if let Some(id) = req.id {
                enqueue(tx, Response::success(id, json!(plugin.list_secret_keys())));
            }
        }
        Ok(Method::WaitUntilDrained) => {
            if let Some(v) = req.params {
                if let Ok(p) = serde_json::from_value::<WaitUntilDrainedParams>(v) { plugin.wait_until_drained(p).await; }
            }
            if let Some(id) = req.id { ok_null!(id); }
        }
        Ok(Method::SetConfig) => {
            if let Some(v) = req.params {
                if let Ok(p) = serde_json::from_value::<SetConfigParams>(v) { plugin.set_config(p).await; }
            }
            if let Some(id) = req.id { ok_null!(id); }
        }
        Ok(Method::SetSecrets) => {
            if let Some(v) = req.params {
                if let Ok(p) = serde_json::from_value::<SetSecretsParams>(v) { plugin.set_secrets(p).await; }
            }
            if let Some(id) = req.id { ok_null!(id); }
        }
        _ => {
            if let Some(id) = req.id {
               enqueue(tx, Response::fail(id, -32601, "Method not found", None));
            }
        }
    }
}


