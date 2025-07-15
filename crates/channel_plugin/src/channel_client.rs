use crate::jsonrpc::{Request, Response};
use crate::message::{ChannelMessage, MessageOutParams};
use crate::plugin_actor::Method;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};

#[async_trait]
pub trait ChannelClientType: Send + Sync {
    /// Send a message **to** the channel (bot → users).
    async fn send(&self, msg: ChannelMessage) -> Result<()>;

    /// Wait for the **next inbound** message (users → bot).
    ///
    /// Returns `None` if the client is permanently closed.
    async fn next_inbound(&mut self) -> Option<ChannelMessage>;
}

#[derive(Clone, Debug)]
pub enum ChannelClient {
    Rpc(RpcChannelClient),
    // Future: WebSocket(WebSocketChannelClient), etc.
}

impl ChannelClient {
    pub fn new(
        tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
        rx_src: broadcast::Sender<ChannelMessage>,
    ) -> Self {
        ChannelClient::Rpc(RpcChannelClient::new(tx, rx_src))
    }
}

#[async_trait]
impl ChannelClientType for ChannelClient {
    async fn send(&self, msg: ChannelMessage) -> Result<()> {
        match self {
            ChannelClient::Rpc(client) => client.send(msg).await,
            // ChannelClient::WebSocket(client) => client.send(msg).await, // future variant
        }
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        match self {
            ChannelClient::Rpc(client) => client.next_inbound().await,
            // ChannelClient::WebSocket(client) => client.next_inbound().await, // future variant
        }
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
    async fn send(&self, msg: ChannelMessage) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::time::{Duration, timeout};

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
        let (client, mut outbound_rx, _msg_tx) = make_client();

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
}
