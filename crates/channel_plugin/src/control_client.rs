use std::sync::Arc;
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;
use tokio::net::TcpStream;
use futures_util::{stream::{SplitSink, SplitStream}, SinkExt, StreamExt};
use crate::{
    jsonrpc::{Id, Request, Response}, message::{
        CapabilitiesResult, ChannelCapabilities, ChannelState, DrainResult, HealthResult, InitParams, InitResult, ListKeysResult, NameResult, StateResult, StopResult, WaitUntilDrainedParams, WaitUntilDrainedResult
    }, plugin_actor::Method, plugin_runtime::PluginHandler, pubsub_client::PubSubPlugin
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::sync::{mpsc, oneshot};

#[async_trait]
pub trait ControlClientType: Send + Sync + 'static {
    async fn name(&self) -> anyhow::Result<String>;
    async fn init(&self, params: InitParams) -> anyhow::Result<InitResult>;
    async fn start(&self, params: InitParams) -> anyhow::Result<InitResult>;
    async fn drain(&self) -> anyhow::Result<DrainResult>;
    async fn stop(&self) -> anyhow::Result<StopResult>;
    async fn state(&self) -> anyhow::Result<ChannelState>;
    async fn health(&self) -> anyhow::Result<HealthResult>;
    async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities>;
    async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult>;
    async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult>;
    async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<WaitUntilDrainedResult>;
    // Possibly: set_config, set_secrets, version, etc.
}
#[derive(Clone, Debug)]
pub enum ControlClient {
    Rpc(RpcControlClient),
    PubSub(PubSubControlClient),
    WebSocket(WebSocketControlClient),
    // Future: Grpc(GrpcControlClient), etc.
}

impl ControlClient {
    pub fn new(tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>) -> Self {
        ControlClient::Rpc(RpcControlClient::new(tx))
    }

    pub async fn new_ws(ws_url: &str) -> Result<Self> {
        let client = WebSocketControlClient::new(ws_url).await?;
        Ok(ControlClient::WebSocket(client))
    }

    pub async fn new_pubsub(router_id: String, greentic_id: String ) -> Result<Self> {
        let pubsub = PubSubPlugin::new(router_id, greentic_id);
        let client = PubSubControlClient::new(pubsub);
        Ok(ControlClient::PubSub(client))
    }

}

#[async_trait]
impl ControlClientType for ControlClient {
    async fn name(&self) -> anyhow::Result<String> {
        match self {
            ControlClient::Rpc(client) => client.name().await,
            ControlClient::WebSocket(client) => client.name().await,
            ControlClient::PubSub(client) => client.name().await,
        }
    }

    async fn init(&self, params: InitParams) -> anyhow::Result<InitResult> {
        match self {
            ControlClient::Rpc(client) => client.init(params).await,
            ControlClient::WebSocket(client) => client.init(params).await,
            ControlClient::PubSub(client) => client.init(params).await,
        }
    }

    async fn start(&self, params: InitParams) -> anyhow::Result<InitResult> {
        match self {
            ControlClient::Rpc(client) => client.start(params).await,
            ControlClient::WebSocket(client) => client.start(params).await,
            ControlClient::PubSub(client) => client.start(params).await,
        }
    }

    async fn drain(&self) -> anyhow::Result<DrainResult> {
        match self {
            ControlClient::Rpc(client) => client.drain().await,
            ControlClient::WebSocket(client) => client.drain().await,
            ControlClient::PubSub(client) => client.drain().await,
        }
    }

    async fn stop(&self) -> anyhow::Result<StopResult> {
        match self {
            ControlClient::Rpc(client) => client.stop().await,
            ControlClient::WebSocket(client) => client.stop().await,
            ControlClient::PubSub(client) => client.stop().await,
        }
    }

    async fn state(&self) -> anyhow::Result<ChannelState> {
        match self {
            ControlClient::Rpc(client) => client.state().await,
            ControlClient::WebSocket(client) => client.state().await,
            ControlClient::PubSub(client) => client.state().await,
        }
    }

    async fn health(&self) -> anyhow::Result<HealthResult> {
        match self {
            ControlClient::Rpc(client) => client.health().await,
            ControlClient::WebSocket(client) => client.health().await,
            ControlClient::PubSub(client) => client.health().await,
        }
    }

    async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
        match self {
            ControlClient::Rpc(client) => client.capabilities().await,
            ControlClient::WebSocket(client) => client.capabilities().await,
            ControlClient::PubSub(client) => client.capabilities().await,
        }
    }

    async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult> {
        match self {
            ControlClient::Rpc(client) => client.list_config_keys().await,
            ControlClient::WebSocket(client) => client.list_config_keys().await,
            ControlClient::PubSub(client) => client.list_config_keys().await,
        }
    }

    async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult> {
        match self {
            ControlClient::Rpc(client) => client.list_secret_keys().await,
            ControlClient::WebSocket(client) => client.list_secret_keys().await,
            ControlClient::PubSub(client) => client.list_secret_keys().await,
        }
    }

    async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<WaitUntilDrainedResult> {
        match self {
            ControlClient::Rpc(client) => client.wait_until_drained(timeout_ms).await,
            ControlClient::WebSocket(client) => client.wait_until_drained(timeout_ms).await,
            ControlClient::PubSub(client) => client.wait_until_drained(timeout_ms).await,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PubSubControlClient {
    plugin: Arc<RwLock<PubSubPlugin>>,
}

impl PubSubControlClient {
    pub fn new(plugin: PubSubPlugin) -> Self {
        Self { plugin: Arc::new(RwLock::new(plugin)) }
    }
}

#[async_trait]
impl ControlClientType for PubSubControlClient {
    async fn name(&self) -> anyhow::Result<String> {
        let plugin = self.plugin.read().await;
        Ok(plugin.name().name)
    }

    async fn init(&self, p: InitParams) -> anyhow::Result<InitResult> {
        let mut plugin = self.plugin.write().await;
        Ok(plugin.init(p).await)
    }

    async fn start(&self, p: InitParams) -> anyhow::Result<InitResult> {
        let mut plugin = self.plugin.write().await;
        Ok(plugin.init(p).await) // reuse `init` as start
    }

    async fn drain(&self) -> anyhow::Result<DrainResult> {
        let mut plugin = self.plugin.write().await;
        Ok(plugin.drain().await)
    }

    async fn stop(&self) -> anyhow::Result<StopResult> {
        let mut plugin = self.plugin.write().await;
        Ok(plugin.stop().await)
    }

    async fn state(&self) -> anyhow::Result<ChannelState> {
        let plugin = self.plugin.read().await;
        Ok(plugin.state().await.state)
    }

    async fn health(&self) -> anyhow::Result<HealthResult> {
        let plugin = self.plugin.read().await;
        Ok(plugin.health().await)
    }

    async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
        let plugin = self.plugin.read().await;
        Ok(plugin.capabilities().capabilities)
    }

    async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult> {
        let plugin = self.plugin.read().await;
        Ok(plugin.list_config_keys())
    }

    async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult> {
        let plugin = self.plugin.read().await;
        Ok(plugin.list_secret_keys())
    }

    async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<WaitUntilDrainedResult> {
        let params = WaitUntilDrainedParams{ timeout_ms };
        let plugin = self.plugin.read().await;
        Ok(plugin.wait_until_drained(params).await)
    }
}

#[derive(Debug, Clone)]
pub struct RpcControlClient {
    /// outbound <Request, responder> channel shared with the actor task
    tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
}

impl RpcControlClient {
    pub fn new(tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>) -> Self {
        Self { tx }
    }

    /// Generic helper: send `method` with optional `params` and
    /// deserialize the JSON-RPC result into `R`.
    async fn call<R>(&self, method: Method, params: Option<serde_json::Value>) -> Result<R>
    where
        R: DeserializeOwned,
    {
        let id = Id::String(Uuid::new_v4().to_string());
        let req = Request::call(id.clone(), method.to_owned(), params.clone());

        let (tx_rsp, rx_rsp) = oneshot::channel();
        self.tx
            .send((req, tx_rsp))
            .await
            .map_err(|_| anyhow!("plugin actor is dead"))?;

        let rsp = rx_rsp
            .await
            .map_err(|_| anyhow!("plugin actor dropped response"))?;

        if let Some(err) = rsp.error {
            return Err(anyhow!("RPC error: {:?}", err));
        }
        let val = rsp.result.ok_or_else(|| anyhow!("missing result field"))?;
        Ok(serde_json::from_value(val)?)
    }
}

#[async_trait]
impl ControlClientType for RpcControlClient {
    async fn name(&self) -> anyhow::Result<String> {
        let name: NameResult = self
            .call::<NameResult>(Method::Name, None)
            .await
            .expect("Could not get name");
        return Ok(name.name);
    }

    async fn init(&self, p: InitParams) -> Result<InitResult> {
        self.call(Method::Init, Some(serde_json::to_value(p)?))
            .await
    }

    async fn start(&self, p: InitParams) -> Result<InitResult> {
        self.call(Method::Start, Some(serde_json::to_value(p)?))
            .await
    }

    async fn drain(&self) -> Result<DrainResult> {
        self.call(Method::Drain, None).await
    }

    async fn stop(&self) -> Result<StopResult> {
        self.call(Method::Stop, None).await
    }

    async fn state(&self) -> Result<ChannelState> {
        let state: StateResult = self
            .call::<StateResult>(Method::State, None)
            .await
            .expect("Could not get state");
        return Ok(state.state);
    }

    async fn health(&self) -> Result<HealthResult> {
        self.call(Method::Health, None).await
    }

    async fn capabilities(&self) -> Result<ChannelCapabilities> {
        let caps: CapabilitiesResult = self
            .call::<CapabilitiesResult>(Method::Capabilities, None)
            .await
            .expect("Could not get capabilities");
        return Ok(caps.capabilities);
    }

    async fn list_config_keys(&self) -> Result<ListKeysResult> {
        self.call(Method::ListConfigKeys, None).await
    }

    async fn list_secret_keys(&self) -> Result<ListKeysResult> {
        self.call(Method::ListSecretKeys, None).await
    }

    async fn wait_until_drained(&self, t_ms: u64) -> Result<WaitUntilDrainedResult> {
        self.call::<WaitUntilDrainedResult>(
            Method::WaitUntilDrained,
            Some(serde_json::json!({ "timeout_ms": t_ms })),
        )
        .await
    }
}


#[derive(Debug, Clone)]
pub struct WebSocketControlClient {
    sender: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>>>,
    receiver: Arc<Mutex<SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>>>,
}

impl WebSocketControlClient {
    pub async fn new(ws_url: &str) -> Result<Self> {
        let (ws_stream, _) = connect_async(ws_url).await?;
        let (write, read) = ws_stream.split();

        Ok(WebSocketControlClient {
            sender: Arc::new(Mutex::new(write)),
            receiver: Arc::new(Mutex::new(read)),
        })
    }

    async fn call<R>(&self, method: Method, params: Option<Value>) -> Result<R>
    where
        R: DeserializeOwned,
    {
        let id = Id::String(Uuid::new_v4().to_string());
        let req = Request::call(id.clone(), method, params);

        let serialized = serde_json::to_string(&req)?;
        {
            let mut sender = self.sender.lock().await;
            sender.send(WsMessage::Text(serialized.into())).await?;
        }

        // Now wait for matching response
        loop {
            let mut receiver = self.receiver.lock().await;
            if let Some(Ok(WsMessage::Text(txt))) = receiver.next().await {
                let rsp: Response = serde_json::from_str(&txt)?;
                if rsp.id == id.clone() {
                    if let Some(err) = rsp.error {
                        return Err(anyhow!("RPC error: {:?}", err));
                    }
                    let val = rsp.result.ok_or_else(|| anyhow!("Missing result field"))?;
                    return Ok(serde_json::from_value(val)?);
                }
            }
        }
    }
}

#[async_trait]
impl ControlClientType for WebSocketControlClient {
    async fn name(&self) -> Result<String> {
        let result: NameResult = self.call(Method::Name, None).await?;
        Ok(result.name)
    }

    async fn init(&self, p: InitParams) -> Result<InitResult> {
        self.call(Method::Init, Some(serde_json::to_value(p)?)).await
    }

    async fn start(&self, p: InitParams) -> Result<InitResult> {
        self.call(Method::Start, Some(serde_json::to_value(p)?)).await
    }

    async fn drain(&self) -> Result<DrainResult> {
        self.call(Method::Drain, None).await
    }

    async fn stop(&self) -> Result<StopResult> {
        self.call(Method::Stop, None).await
    }

    async fn state(&self) -> Result<ChannelState> {
        let result: StateResult = self.call(Method::State, None).await?;
        Ok(result.state)
    }

    async fn health(&self) -> Result<HealthResult> {
        self.call(Method::Health, None).await
    }

    async fn capabilities(&self) -> Result<ChannelCapabilities> {
        let result: CapabilitiesResult = self.call(Method::Capabilities, None).await?;
        Ok(result.capabilities)
    }

    async fn list_config_keys(&self) -> Result<ListKeysResult> {
        self.call(Method::ListConfigKeys, None).await
    }

    async fn list_secret_keys(&self) -> Result<ListKeysResult> {
        self.call(Method::ListSecretKeys, None).await
    }

    async fn wait_until_drained(&self, t_ms: u64) -> Result<WaitUntilDrainedResult> {
        self.call(Method::WaitUntilDrained, Some(json!({ "timeout_ms": t_ms })))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::{accept_async, tungstenite::protocol::Message as WsMessage};
    use tokio::net::TcpListener;
    use futures_util::{SinkExt, StreamExt};
    use std::net::SocketAddr;

    #[tokio::test]
    async fn test_websocket_control_client_name_call() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = TcpListener::bind(&addr).await.unwrap();
        let local_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            while let Some(Ok(WsMessage::Text(txt))) = ws.next().await {
                let req: serde_json::Value = serde_json::from_str(&txt).unwrap();
                if req["method"] == "name" {
                    let id = req["id"].clone();
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "name": "test-plugin" }
                    });
                    let text = serde_json::to_string(&response).unwrap();
                    ws.send(WsMessage::Text(text.into())).await.unwrap();
                }
            }
        });

        let url = format!("ws://{}", local_addr);
        let client = WebSocketControlClient::new(&url).await.unwrap();
        let result = client.name().await.unwrap();
        assert_eq!(result, "test-plugin");
    }

    #[tokio::test]
    async fn test_websocket_control_client_state_call() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = TcpListener::bind(&addr).await.unwrap();
        let local_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            while let Some(Ok(WsMessage::Text(txt))) = ws.next().await {
                let req: serde_json::Value = serde_json::from_str(&txt).unwrap();
                if req["method"] == "state" {
                    let id = req["id"].clone();
                    let state = StateResult{
                        state: ChannelState::RUNNING,
                    };
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": state,
                    });
                    let text = serde_json::to_string(&response).unwrap();
                    ws.send(WsMessage::Text(text.into())).await.unwrap();
                }
            }
        });

        let url = format!("ws://{}", local_addr);
        let client = WebSocketControlClient::new(&url).await.unwrap();
        let result = client.state().await.unwrap();
        assert_eq!(result, ChannelState::RUNNING);
    }
}
