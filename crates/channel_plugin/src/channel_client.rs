use std::sync::Arc;
use async_trait::async_trait;
use crate::jsonrpc::{  Request, Response};
use crate::message::ChannelMessage;
#[cfg(feature = "test-utils")]
use crate::message::MessageOutParams;
#[cfg(feature = "test-utils")]
use crate::plugin_actor::Command;        // the trait from §1
use anyhow::{anyhow,Result};
use tokio::{
    sync::{mpsc, oneshot, broadcast},
};

#[async_trait]
pub trait ChannelClient:  Send + Sync {
    /// Send a message **to** the channel (bot → users).
    async fn send(&self, msg: ChannelMessage) -> Result<()>;

    /// Wait for the **next inbound** message (users → bot).
    ///
    /// Returns `None` if the client is permanently closed.
    async fn next_inbound(&mut self) -> Option<ChannelMessage>;
}

pub trait CloneableChannelClient: ChannelClient {
    fn clone_box(&self) -> Box<dyn CloneableChannelClient>;
}

impl<T> CloneableChannelClient for T
where
    T: ChannelClient + Clone + Send + 'static,
{
    fn clone_box(&self) -> Box<dyn CloneableChannelClient> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn CloneableChannelClient> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}


#[derive( Debug, )]
pub struct RpcChannelClient {
    // ––– outbound request channel –––
    tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
    // ––– inbound message notifications ––
    rx: broadcast::Receiver<ChannelMessage>,
    rx_src: Arc<broadcast::Sender<ChannelMessage>>,
}

impl CloneableChannelClient for RpcChannelClient {
    fn clone_box(&self) ->  Box<dyn CloneableChannelClient> {
        Box::new(Self {
            tx: self.tx.clone(),
            rx: self.rx_src.subscribe(), // 🔁 Fresh receiver
            rx_src: self.rx_src.clone(),
        })
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
        self.tx.send((req, tx_rsp)).await
            .map_err(|_| anyhow!("Plugin actor is dead"))?;
        rx_rsp.await.map_err(|_| anyhow!("Plugin actor dropped response"))
    }
    
    pub async fn rpc_notify(
        &self,
        req: Request,
    ) -> anyhow::Result<()> {

        // Just fire and forget — use a dummy oneshot::Sender
        let (_tx, _rx) = tokio::sync::oneshot::channel();
        self.tx.send((req, _tx)).await
            .map_err(|_| anyhow::anyhow!("ActorHandle is dead"))?;

        Ok(())
    }
}


#[async_trait::async_trait]
impl ChannelClient for RpcChannelClient {
    async fn send(&self, msg: ChannelMessage) -> Result<()> {
        let params = serde_json::to_value(msg)?;
        let r = Request::notification("messageOut", Some(params));
        self.rpc_notify(r).await
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        loop {
            match self.rx.recv().await {
                Ok(msg) => return Some(msg),
                Err(broadcast::error::RecvError::Lagged(_)) => continue, // just skip lag
                Err(_) => return None, // channel closed
            }
        }
    }
}

#[async_trait::async_trait]
impl ChannelClient for Box<dyn CloneableChannelClient> {
    async fn send(&self, msg: ChannelMessage) -> anyhow::Result<()> {
        (**self).send(msg).await
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        (**self).next_inbound().await
    }
}


#[cfg(feature = "test-utils")]
pub struct InProcChannelClient {
    // slow-path control to the actor
    cmd_tx: mpsc::Sender<Command>,
    // fast inbound queue (each clone gets its *own* receiver)
    _msg_rx: broadcast::Receiver<ChannelMessage>,
    msg_rx_src: Arc<broadcast::Sender<ChannelMessage>>,
}
#[cfg(feature = "test-utils")]
impl InProcChannelClient {
    pub fn new(cmd_tx: mpsc::Sender<Command>,msg_rx: broadcast::Receiver<ChannelMessage>,msg_rx_src: Arc<broadcast::Sender<ChannelMessage>>) -> Self {
        Self {
            cmd_tx,
            _msg_rx: msg_rx,
            msg_rx_src,
        }
    }
}
#[cfg(feature = "test-utils")]
impl CloneableChannelClient for InProcChannelClient {

    
    fn clone_box(&self) -> Box<dyn CloneableChannelClient> {
        Box::new(Self {
            cmd_tx: self.cmd_tx.clone(),
            _msg_rx: self.msg_rx_src.subscribe(), // 🔁 Fresh receiver
            msg_rx_src: self.msg_rx_src.clone(),
        })
    }
}

#[async_trait]
#[cfg(feature = "test-utils")]
impl ChannelClient for InProcChannelClient {
    async fn send(&self, msg: ChannelMessage) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(Command::SendMessage(MessageOutParams { message: msg }, tx))
            .await?;
        let res = rx.await?;
        if res.success { Ok(()) } else { Err(anyhow::anyhow!("channel rejected message")) }
    }

    async fn next_inbound(&mut self) -> Option<ChannelMessage> {
        let mut subscribe = self.msg_rx_src.subscribe();
        match subscribe.recv().await {
            Ok(msg) => Some(msg),
            Err(err) => {
                tracing::error!("Could not get next message because {:?}",err.to_string());
                None
            },
        }
    }
}


#[cfg(test)]
mod tests {
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
        (RpcChannelClient::new(tx.clone(), msg_tx.clone()), rx, msg_tx)
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
        assert_eq!(req.params, Some(json!(msg)));
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
