// crates/ws_plugin/src/main.rs
use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex as SMutex},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{
        Mutex, broadcast,
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    },
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message as WsMsg};

use channel_plugin::{
    message::{
        CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult,
        HealthResult, InitResult, ListKeysResult, MessageContent, MessageInResult,
        MessageOutParams, MessageOutResult, NameResult, StateResult, StopResult, make_session_key,
    },
    plugin_helpers::{
        build_text_message, build_user_joined_event, build_user_left_event,
        get_user_joined_left_events,
    },
    plugin_runtime::{HasStore, PluginHandler, run},
};
use tracing::{debug, error, info};

/// Internal manager commands
enum Command {
    Register {
        peer: String,
        tx: UnboundedSender<WsMsg>,
    },
    Unregister {
        peer: String,
    },
    Send {
        peer: String,
        msg: WsMsg,
    },
}

/// WebSocket channel plugin
#[derive(Debug, Clone)]
pub struct WsPlugin {
    addr: String,
    conn_handle: Option<Arc<JoinHandle<()>>>,
    manager_handle: Option<Arc<JoinHandle<()>>>,
    state: Arc<SMutex<ChannelState>>,
    cmd_tx: Option<UnboundedSender<Command>>,
    inbound_tx: UnboundedSender<ChannelMessage>, // to Greentic
    inbound_rx: Arc<Mutex<UnboundedReceiver<ChannelMessage>>>,
    shutdown_tx: Option<broadcast::Sender<()>>,
    cfg: DashMap<String, String>,
    secrets: DashMap<String, String>,
}

impl Default for WsPlugin {
    fn default() -> Self {
        let (inbound_tx, inbound_rx) = unbounded_channel::<ChannelMessage>();
        Self {
            addr: "0.0.0.0:8888".into(),
            state: Arc::new(SMutex::new(ChannelState::STOPPED)),
            cmd_tx: None,
            inbound_tx,
            inbound_rx: Arc::new(Mutex::new(inbound_rx)),
            conn_handle: None,
            manager_handle: None,
            shutdown_tx: None,
            cfg: DashMap::new(),
            secrets: DashMap::new(),
        }
    }
}

// ---------------- manager + connection helpers ----------------

async fn manager_loop(
    name: String,
    mut cmd_rx: UnboundedReceiver<Command>,
    inbound: UnboundedSender<ChannelMessage>,
    mut shutdown: broadcast::Receiver<()>,
) {
    let peers = DashMap::<String, UnboundedSender<WsMsg>>::new();
    loop {
        tokio::select! {
            _ = shutdown.recv() => break,
            Some(cmd) = cmd_rx.recv() => match cmd {
                Command::Register{peer,tx} => {
                    info!("[ws] New connection: {:?}",peer);
                    peers.insert(peer.clone(), tx);
                    let session = make_session_key(&name, &peer);
                    let join_msg = build_user_joined_event(&name, &peer, Some(session));
                    let _ = inbound.send(join_msg);

                }
                Command::Unregister{peer}  => {
                    info!("[ws] Disconnected: {:?}",peer.clone());
                    peers.remove(&peer);
                    let session = make_session_key(&name, &peer);
                    let leave_msg = build_user_left_event(&name, &peer, Some(session));
                    let _ = inbound.send(leave_msg);

                }
                Command::Send{peer,msg}    => {
                    if let Some(tx) = peers.get(&peer) {
                        let _ = tx.send(msg);
                    }
                }
            }
        }
    }
}

fn spawn_conn(
    stream: TcpStream,
    peer: SocketAddr,
    cmd_tx: UnboundedSender<Command>,
    msg_tx: UnboundedSender<ChannelMessage>,
    mut shutdown: broadcast::Receiver<()>,
) {
    tokio::spawn(async move {
        let peer_id = peer.to_string();
        let (mut ws_tx, mut ws_rx) = match accept_async(stream).await {
            Ok(ws) => ws.split(),
            Err(e) => {
                error!("[ws] handshake error: {e}");
                return;
            }
        };
        let (out_tx, mut out_rx) = unbounded_channel::<WsMsg>();

        let _ = cmd_tx.send(Command::Register {
            peer: peer_id.clone(),
            tx: out_tx,
        });

        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    info!("[ws] Shutting down peer {}",peer_id);
                    break;
                },
                Some(frame) = ws_rx.next() => match frame {
                    Ok(WsMsg::Text(txt)) => {
                        info!("[ws] received {:?}", txt.as_str());
                        // Build a messageIn JSON-RPC request and emit
                        let msg = build_text_message(
                            &peer_id,    // from
                            Some(make_session_key("ws", &peer_id)),
                            "ws",           // channel
                            &txt,
                        );
                        let _ = msg_tx.send(msg);
                    }
                    _ => break,
                },
                Some(out) = out_rx.recv() => {
                    let _ = ws_tx.send(out.clone()).await;
                    info!("[ws] received out {:?}", out);
                }
            }
        }

        let _ = cmd_tx.send(Command::Unregister { peer: peer_id });
    });
}

// ---------------- PluginHandler implementation ----------------

impl HasStore for WsPlugin {
    fn config_store(&self) -> &DashMap<String, String> {
        &self.cfg
    }
    fn secret_store(&self) -> &DashMap<String, String> {
        &self.secrets
    }
}

#[async_trait]
impl PluginHandler for WsPlugin {
    async fn init(&mut self, _p: channel_plugin::message::InitParams) -> InitResult {
        let (cmd_tx, cmd_rx) = unbounded_channel::<Command>();
        self.cmd_tx = Some(cmd_tx.clone());

        let address = self
            .get_config("WS_ADDRESS")
            .unwrap_or_else(|| "0.0.0.0".to_string());

        let port = self
            .get_config("WS_PORT")
            .unwrap_or_else(|| "8888".to_string());

        self.addr = format!("{}:{}", address, port);

        // manager loop for fan-out
        let listener = match TcpListener::bind(&self.addr).await {
            Ok(l) => l,
            Err(e) => {
                return InitResult {
                    success: false,
                    error: Some(format!("cannot bind {}: {e}", self.addr)),
                };
            }
        };
        let (shutdown_tx, _) = broadcast::channel::<()>(1);
        let inbound_tx = self.inbound_tx.clone();
        self.shutdown_tx = Some(shutdown_tx.clone());
        let name = self.name().clone();
        let manager = tokio::spawn(manager_loop(
            name.name,
            cmd_rx,
            inbound_tx.clone(),
            shutdown_tx.subscribe(),
        ));

        self.manager_handle = Some(Arc::new(manager));
        info!("[ws] listening on {}", self.addr);
        // Start accept loop once
        let this = self.clone();

        let handle = tokio::spawn(async move {
            loop {
                let cmd_tx_clone = this.cmd_tx.clone().expect("cmd_tx should have been set");
                let inbound_tx_clone = this.inbound_tx.clone();
                let (stream, peer) = listener.accept().await.expect("accept");
                spawn_conn(
                    stream,
                    peer,
                    cmd_tx_clone,
                    inbound_tx_clone,
                    shutdown_tx.subscribe(),
                );
            }
        });

        self.conn_handle = Some(Arc::new(handle));
        *self.state.lock().unwrap() = ChannelState::RUNNING;

        InitResult {
            success: true,
            error: None,
        }
    }

    // Greentic → Plugin : deliver outbound
    async fn send_message(&mut self, p: MessageOutParams) -> MessageOutResult {
        if let Some(MessageContent::Text { text }) = p.message.content.get(0) {
            let peer = p
                .message
                .to
                .get(0) // first recipient
                .map(|rcpt| rcpt.id.clone())
                .or_else(|| Some(p.message.from.id.clone())) // fallback
                .unwrap();
            if let Some(sender) = &self.cmd_tx {
                info!("[ws] send {:?}", text);
                let _ = sender.send(Command::Send {
                    peer: peer.clone(),
                    msg: WsMsg::Text(text.into()),
                });
            } else {
                return MessageOutResult {
                    success: false,
                    error: Some("plugin not initialised".into()),
                };
            }
        }
        MessageOutResult {
            success: true,
            error: None,
        }
    }

    // Plugin → Greentic : inbound from channel
    async fn receive_message(&mut self) -> MessageInResult {
        let mut inbound = self.inbound_rx.lock().await;
        match inbound.recv().await {
            Some(msg) => {
                info!(
                    "[ws] message_in {} for session {:?}",
                    msg.id, msg.session_id
                );
                MessageInResult {
                    message: msg,
                    error: false,
                }
            }
            None => {
                error!("[ws] empty message_in");
                // The sending half was dropped – return an “empty” answer so
                // the runtime doesn’t panic. (You could also shut the plugin
                // down here.)
                MessageInResult {
                    message: ChannelMessage::default(), // tiny helper shown earlier
                    error: true,
                }
            }
        }
    }

    /// Drain the plugin
    async fn drain(&mut self) -> DrainResult {
        info!("[ws] draining");
        *self.state.lock().unwrap() = ChannelState::DRAINING;
        if let Some(shutdown) = &self.shutdown_tx {
            debug!("[ws] shutdown");
            let _ = shutdown.send(()); // notify all listeners
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Some(handle) = &self.manager_handle {
            debug!("[ws] manager_handle");
            handle.abort();
        }
        if let Some(handle) = &self.conn_handle {
            debug!("[ws] conn_handle");
            handle.abort();
        }

        *self.state.lock().unwrap() = ChannelState::STOPPED;
        DrainResult {
            success: true,
            error: None,
        }
    }
    /// Stop the plugin
    async fn stop(&mut self) -> StopResult {
        info!("[ws] Stop called");
        self.drain().await;
        StopResult {
            success: true,
            error: None,
        }
    }

    /// Check the health of the plugin
    async fn health(&self) -> HealthResult {
        HealthResult {
            healthy: true,
            reason: None,
        }
    }
    /// Request the current status
    async fn state(&self) -> StateResult {
        StateResult {
            state: self.state.lock().unwrap().clone(),
        }
    }

    /// Returns the plugin name, e.g., "telegram", "ws", etc.
    fn name(&self) -> NameResult {
        NameResult { name: "ws".into() }
    }
    /// List of expected config keys (like `API_KEY`, `WS_PORT`, etc.)
    fn list_config_keys(&self) -> ListKeysResult {
        let keys = vec![
            (
                "WS_ADDRESS".into(),
                Some("websocket address to list on. If not set '0.0.0.0' will be used.".into()),
            ),
            (
                "WS_PORT".into(),
                Some("websocket port to list on. If not set '8888' will be used.".into()),
            ),
        ];
        ListKeysResult {
            required_keys: vec![],
            optional_keys: keys,
            dynamic_keys: vec![],
        }
    }
    /// List of expected secret keys
    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![],
            optional_keys: vec![],
            dynamic_keys: vec![],
        }
    }
    /// Declares plugin capabilities (sending, receiving, text, etc.)
    fn capabilities(&self) -> CapabilitiesResult {
        CapabilitiesResult {
            capabilities: ChannelCapabilities {
                name: "ws".into(),
                supports_sending: true,
                supports_receiving: true,
                supports_text: true,
                supports_routing: true,
                supported_events: get_user_joined_left_events(),
                ..Default::default()
            },
        }
    }
}

// ---------------- main ----------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(WsPlugin::default()).await
}
/*
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut plugin = WsPlugin::default();
    let params = channel_plugin::message::InitParams {
        version: "0".into(),
        config: [("WS_ADDRESS".into(), "127.0.0.1".into()), ("WS_PORT".into(), "8888".into())]
            .iter()
            .cloned()
            .collect(),
        secrets: Default::default(),
        log_level: channel_plugin::message::LogLevel::Info,
        log_dir: None,
        otel_endpoint: None,
    };
    plugin.start(params).await;
    run(plugin).await
}

*/

#[cfg(test)]
mod tests {
    use super::*;
    use channel_plugin::message::*;
    use tokio::time::Duration;
    use tokio_tungstenite::{connect_async, tungstenite::protocol::Message as WsMessage};

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn start_ws_plugin(port: u16) -> WsPlugin {
        let mut plugin = WsPlugin::default();
        let params = InitParams {
            version: "test".into(),
            config: vec![
                ("WS_ADDRESS".into(), "127.0.0.1".into()),
                ("WS_PORT".into(), port.to_string()),
            ],
            secrets: vec![],
            log_level: LogLevel::Info,
            log_dir: Some("./logs".into()),
            otel_endpoint: None,
        };
        assert!(plugin.start(params).await.success);
        plugin
    }

    #[tokio::test]
    async fn test_bind_and_accept() {
        let port = free_port();
        info!("Test port: {}", port);
        let _plugin = start_ws_plugin(port).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("Could not connect to plugin");
        drop(stream);
    }

    #[tokio::test]
    async fn test_send_and_receive_message() {
        let port = free_port();
        let mut plugin = start_ws_plugin(port).await;
        // Wait for listener to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let url = format!("ws://127.0.0.1:{port}");
        let (mut ws_stream, _) = connect_async(&url).await.unwrap();

        let input_text = "test message";
        ws_stream
            .send(WsMessage::Text(input_text.into()))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let time_left = deadline.saturating_duration_since(tokio::time::Instant::now());

            if time_left == std::time::Duration::ZERO {
                // ✅ Shutdown plugin after test
                plugin.drain().await;
                panic!("Timed out waiting for text message");
            }

            let recv = tokio::time::timeout(time_left, plugin.receive_message()).await;

            match recv {
                Ok(incoming) => {
                    if let Some(MessageContent::Text { text }) = incoming.message.content.get(0) {
                        assert_eq!(text, input_text);
                        break;
                    } else {
                        // Skip non-text messages like UserJoined
                        continue;
                    }
                }
                Err(_) => {
                    plugin.drain().await;
                    panic!("Timed out waiting for plugin message")
                }
            }
        }

        // ✅ Shutdown plugin after test
        plugin.drain().await;
    }

    #[tokio::test]
    async fn test_plugin_drain() {
        let port = free_port();
        let mut plugin = start_ws_plugin(port).await;
        plugin.stop().await;

        let result = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
        assert!(
            result.is_err(),
            "Port should be released after plugin stops"
        );
    }
}
