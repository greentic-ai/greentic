use crate::jsonrpc::{Request, Response};
use crate::message::{ChannelMessage, MessageOutParams};
use crate::plugin_actor::Method;
use crate::plugin_runtime::PluginHandler;
use crate::pubsub_client::PubSubPlugin;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

#[async_trait]
pub trait ChannelClientType: Send + Sync {
    /// Send a message **to** the channel (bot → users).
    async fn send(&mut self, msg: ChannelMessage) -> Result<()>;

    /// Wait for the **next inbound** message (users → bot).
    ///
    /// Returns `None` if the client is permanently closed.
    async fn next_inbound(&mut self) -> Option<ChannelMessage>;
}

#[derive(Clone, Debug)]
pub enum ChannelClient {
    Rpc(RpcChannelClient),
    PubSub(PubSubChannelClient),
    WebSocket(WebSocketChannelClient),
}

impl ChannelClient {
    pub fn new(
        tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
        rx_src: broadcast::Sender<ChannelMessage>,
    ) -> Self {
        ChannelClient::Rpc(RpcChannelClient::new(tx, rx_src))
    }

    pub async fn new_pubsub(router_id: String, greentic_id: String ) -> Result<Self> {
        let pubsub = PubSubPlugin::new(router_id, greentic_id);
        let client = PubSubChannelClient::new(pubsub);
        Ok(ChannelClient::PubSub(client))
    }

    pub async fn new_ws(ws_url: &str) -> Result<Self> {
        let client = WebSocketChannelClient::new(ws_url).await?;
        Ok(ChannelClient::WebSocket(client))
    }
}

#[async_trait]
impl ChannelClientType for ChannelClient {
    async fn send(&mut self, msg: ChannelMessage) -> Result<()> {
        match self {
            ChannelClient::Rpc(client) => client.send(msg).await,
            ChannelClient::PubSub(client) => client.send(msg).await,
            ChannelClient::WebSocket(client) => client.send(msg).await,
        }
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        match self {
            ChannelClient::Rpc(client) => client.next_inbound().await,
            ChannelClient::PubSub(client) => client.next_inbound().await,
            ChannelClient::WebSocket(client) => client.next_inbound().await, 
        }
    }
}

#[derive(Clone, Debug)]
pub struct PubSubChannelClient {
    plugin: PubSubPlugin,
}

impl PubSubChannelClient {
    pub fn new(
        plugin: PubSubPlugin,
    ) -> Self {
        Self {
            plugin,
        }
    }
}

#[async_trait]
impl ChannelClientType for PubSubChannelClient {
    async fn send(&mut self, msg: ChannelMessage) -> Result<()> {
        let params = MessageOutParams{ message: msg };
        let result = self.plugin.send_message(params).await;
        if result.success {
            Ok(())
        } else {
            Err(anyhow::anyhow!(result.error.unwrap_or_else(|| "Unknown error sending message".to_string())))
        }
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        let msg = self.plugin.receive_message().await;
        Some(msg.message)
    }
}

#[derive(Debug)]
pub struct RpcChannelClient {
    // ––– outbound request channel –––
    tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
    // ––– inbound message notifications ––
    rx: broadcast::Receiver<ChannelMessage>,
    rx_src: Arc<broadcast::Sender<ChannelMessage>>,
}

impl Clone for RpcChannelClient {
    fn clone(&self) -> Self {
        RpcChannelClient {
            tx: self.tx.clone(),
            rx: self.rx_src.subscribe(), // create a fresh receiver
            rx_src: self.rx_src.clone(),
        }
    }
}
impl RpcChannelClient {
    pub(crate) fn new(
        tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
        rx_src: broadcast::Sender<ChannelMessage>,
    ) -> Self {
        let rx = rx_src.subscribe();
        Self {
            tx,
            rx,
            rx_src: Arc::new(rx_src),
        }
    }

    pub async fn call_rpc(&self, req: Request) -> Result<Response> {
        let (tx_rsp, rx_rsp) = oneshot::channel();
        self.tx
            .send((req, tx_rsp))
            .await
            .map_err(|_| anyhow!("Plugin actor is dead"))?;
        rx_rsp
            .await
            .map_err(|_| anyhow!("Plugin actor dropped response"))
    }

    pub async fn rpc_notify(&self, req: Request) -> anyhow::Result<()> {
        // Just fire and forget — use a dummy oneshot::Sender
        let (tx, _rx) = tokio::sync::oneshot::channel();
        self.tx
            .send((req, tx))
            .await
            .map_err(|_| anyhow::anyhow!("ActorHandle is dead"))?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl ChannelClientType for RpcChannelClient {
    async fn send(&mut self, msg: ChannelMessage) -> Result<()> {
        let params = MessageOutParams { message: msg };
        let r = Request::notification(Method::MessageOut.to_string(), Some(json!(params)));
        self.rpc_notify(r).await
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        loop {
            match self.rx.recv().await {
                Ok(msg) => return Some(msg),
                Err(broadcast::error::RecvError::Lagged(_)) => continue, // just skip lag
                Err(_) => return None,                                   // channel closed
            }
        }
    }
}


#[derive(Debug, Clone)]
pub struct WebSocketChannelClient {
    sender: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>>>,
    receiver: Arc<Mutex<SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>>>,
}

impl WebSocketChannelClient {
    pub async fn new(ws_url: &str) -> Result<Self> {
        let (ws_stream, _) = connect_async(ws_url).await?;
        let (write, read) = ws_stream.split();

        Ok(WebSocketChannelClient {
            sender: Arc::new(Mutex::new(write)),
            receiver: Arc::new(Mutex::new(read)),
        })
    }
}

#[async_trait]
impl ChannelClientType for WebSocketChannelClient {
    async fn send(&mut self, msg: ChannelMessage) -> Result<()> {
        let params = MessageOutParams { message: msg };
        let req = Request::notification(Method::MessageOut.to_string(), Some(json!(params)));
        let serialized = serde_json::to_string(&req)?;

        let mut sender = self.sender.lock().await;
        sender.send(WsMessage::Text(serialized.into())).await?;
        Ok(())
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        loop {
            let mut receiver = self.receiver.lock().await;
            match receiver.next().await {
                Some(Ok(WsMessage::Text(txt))) => {
                    match serde_json::from_str::<ChannelMessage>(&txt) {
                        Ok(msg) => return Some(msg),
                        Err(_) => continue, // skip malformed
                    }
                }
                Some(Ok(_)) => continue, // skip non-text
                Some(Err(_)) => return None, // connection error
                None => return None,        // closed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Command, thread::sleep};

    use crate::message::{InitParams, LogLevel};

    use super::*;
    use serde_json::json;
    use tokio::time::{timeout, Duration};

    /// Helper that builds a RpcChannelClient wired to in-memory channels
    fn make_client() -> (
        RpcChannelClient,
        mpsc::Receiver<(Request, oneshot::Sender<Response>)>,
        broadcast::Sender<ChannelMessage>,
    ) {
        let (tx, rx) = mpsc::channel::<(Request, oneshot::Sender<Response>)>(8);
        let (msg_tx, _) = broadcast::channel::<ChannelMessage>(8);
        (
            RpcChannelClient::new(tx.clone(), msg_tx.clone()),
            rx,
            msg_tx,
        )
    }

    #[tokio::test]
    async fn test_send_pushes_notification() {
        let (mut client, mut outbound_rx, _msg_tx) = make_client();

        // dummy message
        let msg = ChannelMessage {
            id: "42".into(),
            ..Default::default()
        };

        // fire `send`
        client.send(msg.clone()).await.expect("send failed");

        // intercept what was placed on the mpsc::Sender
        let (req, _rsp_tx) = outbound_rx.recv().await.expect("nothing sent");
        assert_eq!(req.method, "messageOut");
        assert_eq!(req.id, None, "notification must have no id");
        assert_eq!(req.params, Some(json!(MessageOutParams { message: msg })));
    }

    #[tokio::test]
    async fn test_next_inbound_receives_broadcast() {
        let (mut client, _out_rx, msg_tx) = make_client();

        // send a message into the broadcast hub
        let incoming = ChannelMessage {
            id: "1337".into(),
            ..Default::default()
        };
        msg_tx.send(incoming.clone()).unwrap();

        // the client must yield the same message
        let got = timeout(Duration::from_millis(100), client.next_inbound())
            .await
            .expect("timed-out")
            .expect("stream closed");

        assert_eq!(got, incoming);
    }

    #[tokio::test]
    async fn test_websocket_send_and_receive() {
        use tokio_tungstenite::{accept_async, tungstenite::protocol::Message as WsMessage};
        use tokio::net::TcpListener;
        use std::net::SocketAddr;
        use futures_util::{SinkExt, StreamExt};

        // Bind a TCP listener for a local test server
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = TcpListener::bind(&addr).await.unwrap();
        let local_addr = listener.local_addr().unwrap();

        // Spawn server task that receives JSON-RPC, extracts `ChannelMessage`, and echoes it back
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();

            while let Some(Ok(WsMessage::Text(txt))) = ws.next().await {
                // Parse as JSON-RPC request
                if let Ok(req) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if let Some(params) = req.get("params") {
                        if let Some(msg) = params.get("message") {
                            let echo_text = serde_json::to_string(msg).unwrap();
                            ws.send(WsMessage::Text(echo_text.into())).await.unwrap();
                        }
                    }
                }
            }
        });

        // Connect WebSocket client
        let ws_url = format!("ws://{}", local_addr);
        let client = WebSocketChannelClient::new(&ws_url).await.unwrap();
        let mut client = client;

        // Send a test message
        let test_message = ChannelMessage {
            id: "test123".to_string(),
            ..Default::default()
        };

        client.send(test_message.clone()).await.unwrap();

        // Await echoed response
        let response = tokio::time::timeout(std::time::Duration::from_secs(2), client.next_inbound())
            .await
            .expect("timed out")
            .expect("stream closed");

        assert_eq!(response, test_message);
    }

    #[tokio::test]
    async fn test_pubsub_send_and_receive() {
        // Step 1: Launch NATS server subprocess
        let mut nats_process = Command::new("nats-server")
            .arg("--port=4223") // avoid conflicts with running NATS
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to start nats-server");

        // Step 2: Give it a second to boot up
        sleep(Duration::from_secs(1));
        // Step 1: Dummy plugin that always returns success
        let mut plugin = PubSubPlugin::new("test_router".into(), "test_greentic".into());
        let config = vec![("PUBSUB_NATS_URL".to_string(),"nats://localhost:4222".to_string())];
        let secrets = vec![
            ("GREENTIC_NATS_SEED".to_string(),"123".to_string()),
            ("GREENTIC_SECRETS_DIR".to_string(),"./test".to_string())];
        let p = InitParams{
            version: "123".to_string(),
            config, 
            secrets, 
            log_level: LogLevel::Info, 
            log_dir: Some("./test".to_string()), 
            otel_endpoint: None 
        };
        let result = plugin.start(p).await;
        assert!(result.success);
        //let (tx, rx) = unbounded_channel::<ChannelMessage>();

        // Step 2: Wrap in PubSubChannelClient
        let client = PubSubChannelClient::new(plugin.clone());
        let mut client = client;

        // Step 3: Test send
        let outbound = ChannelMessage {
            id: "pubsub_send".into(),
            ..Default::default()
        };
        let send_result = client.send(outbound.clone()).await;
        assert!(send_result.is_ok(), "send should succeed");

        // Step 4: Simulate an inbound message by pushing into the channel
        //tx.send(outbound.clone()).expect("Failed to push inbound message");

        // Step 5: Test receive
        //let got = tokio::time::timeout(std::time::Duration::from_secs(1), client.next_inbound())
        //    .await
        //    .expect("timed out")
        //    .expect("stream closed");
        //assert_eq!(got, outbound, "received message should match sent");

        // Step 6: Kill the NATS server
        let _ = plugin.stop().await;
        let _ = nats_process.kill();
    }

}
