use crate::{
    jsonrpc::{Id, Request, Response},
    message::{
        CapabilitiesResult, ChannelCapabilities, ChannelState, DrainResult, HealthResult,
        InitParams, InitResult, ListKeysResult, NameResult, StateResult, StopResult,
        WaitUntilDrainedResult,
    },
    plugin_actor::Method,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

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
    // Future: Grpc(GrpcControlClient), etc.
}

impl ControlClient {
    pub fn new(tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>) -> Self {
        ControlClient::Rpc(RpcControlClient::new(tx))
    }
}

#[async_trait]
impl ControlClientType for ControlClient {
    async fn name(&self) -> anyhow::Result<String> {
        match self {
            ControlClient::Rpc(client) => client.name().await,
        }
    }

    async fn init(&self, params: InitParams) -> anyhow::Result<InitResult> {
        match self {
            ControlClient::Rpc(client) => client.init(params).await,
        }
    }

    async fn start(&self, params: InitParams) -> anyhow::Result<InitResult> {
        match self {
            ControlClient::Rpc(client) => client.start(params).await,
        }
    }

    async fn drain(&self) -> anyhow::Result<DrainResult> {
        match self {
            ControlClient::Rpc(client) => client.drain().await,
        }
    }

    async fn stop(&self) -> anyhow::Result<StopResult> {
        match self {
            ControlClient::Rpc(client) => client.stop().await,
        }
    }

    async fn state(&self) -> anyhow::Result<ChannelState> {
        match self {
            ControlClient::Rpc(client) => client.state().await,
        }
    }

    async fn health(&self) -> anyhow::Result<HealthResult> {
        match self {
            ControlClient::Rpc(client) => client.health().await,
        }
    }

    async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
        match self {
            ControlClient::Rpc(client) => client.capabilities().await,
        }
    }

    async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult> {
        match self {
            ControlClient::Rpc(client) => client.list_config_keys().await,
        }
    }

    async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult> {
        match self {
            ControlClient::Rpc(client) => client.list_secret_keys().await,
        }
    }

    async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<WaitUntilDrainedResult> {
        match self {
            ControlClient::Rpc(client) => client.wait_until_drained(timeout_ms).await,
        }
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
