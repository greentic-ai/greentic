use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::Mutex;
use crate::jsonrpc::{ Message, Request, Response};
use crate::message::{ChannelMessage, MessageInResult, MessageOutParams};
use crate::plugin_actor::Command;        // the trait from §1
use anyhow::{anyhow,Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command as TokioCommand,
    sync::{mpsc, oneshot, broadcast},
};

#[async_trait]
pub trait ChannelClient: Send + Sync {
    /// Send a message **to** the channel (bot → users).
    async fn send(&self, msg: ChannelMessage) -> Result<()>;

    /// Wait for the **next inbound** message (users → bot).
    ///
    /// Returns `None` if the client is permanently closed.
    async fn next_inbound(&mut self) -> Option<ChannelMessage>;
}


#[derive( Debug, )]
pub struct RpcChannelClient {
    // ––– outbound request channel –––
    tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
    // ––– inbound message notifications ––
    rx: broadcast::Receiver<ChannelMessage>,
    rx_src: Arc<broadcast::Sender<ChannelMessage>>,
}

impl Clone for RpcChannelClient {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            rx: self.rx_src.subscribe(), // 🔁 Fresh receiver
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
        self.tx.send((req, tx_rsp)).await
            .map_err(|_| anyhow!("Plugin actor is dead"))?;
        rx_rsp.await.map_err(|_| anyhow!("Plugin actor dropped response"))
    }
    /* 
    /// Small generic helper: call any JSON-RPC method and deserialize the
    /// `.result` into the requested type.
    async fn rpc_call<T: DeserializeOwned>(
        &self,
        method: Method,
        params: Option<Value>,
    ) -> anyhow::Result<T> {
        // 1. build request
        let req = Request::call(
            Id::String(Uuid::new_v4().to_string()),
            method.to_string(),
            params,
        );

        // 2. round-trip via existing `call`
        let rsp = self.call_rpc(req).await?;

        // 3. convert the generic `Value` into concrete type `T`
        let v = rsp.result.ok_or_else(|| anyhow!("no result field"))?;
        Ok(serde_json::from_value(v)?)
    }
*/
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

pub async fn spawn_plugin<P: AsRef<Path>>(exe: P)
        -> anyhow::Result<RpcChannelClient> {

    // ── launch ───────────────────────────────────────────────────────
    let mut child = TokioCommand::new(exe.as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin  = child.stdin.take().expect("stdin unavailable");
    let  stdout    = child.stdout.take().expect("stdout unavailable");

    // ── actor queue (JSON-RPC <Request, Response>) ───────────────────
    let (rpc_tx, mut rpc_rx) = mpsc::channel::<(Request, oneshot::Sender<Response>)>(32);
    let (msg_tx, _) = broadcast::channel::<ChannelMessage>(32);
    // track in-flight calls by encoded `id`
    let inflight: Arc<DashMap<String, oneshot::Sender<Response>>> = Arc::new(DashMap::new());
    {
        let inflight = Arc::clone(&inflight);
        // ── task that proxies rx → child.stdin ───────────────────────────
        tokio::spawn(async move {
            while let Some((req, rsp_tx)) = rpc_rx.recv().await {
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
    {
        let inflight = Arc::clone(&inflight);
        let msg_tx_clone = msg_tx.clone();
        tokio::spawn(async move {
            let mut rdr = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = rdr.next_line().await {
                if line.trim().is_empty() { continue; }
                match serde_json::from_str::<Message>(&line) {
                    Ok(Message::Response(rsp)) => {
                        let key = serde_json::to_string(&rsp.id).unwrap();
                        if let Some((_, tx_rsp)) = inflight.remove(&key) { let _ = tx_rsp.send(rsp); }
                    }
                    /* plugin sent a *request* (usually messageIn)            */
                    Ok(Message::Request(req)) if req.method == "messageIn" => {
                        if let Some(p) = req.params {
                            if let Ok(r) = serde_json::from_value::<MessageInResult>(p) {
                                let _ = msg_tx_clone.send(r.message);
                            }
                        }
                    }
                    _ => {}   // bad line ⇒ ignore
                }
            }
            let _ = child.kill().await;
        });
    }

    Ok(RpcChannelClient::new(rpc_tx, msg_tx))

}

#[derive(Clone)]
#[cfg(feature = "test-utils")]
pub struct InProcChannelClient {
    // slow-path control to the actor
    pub cmd_tx: mpsc::Sender<Command>,
    // fast inbound queue (each clone gets its *own* receiver)
    pub in_rx: Arc<Mutex<mpsc::Receiver<ChannelMessage>>>,
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
        self.in_rx.lock().await.recv().await
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
