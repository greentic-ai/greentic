use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::sync::{mpsc, oneshot};
use anyhow::{anyhow, Result};
use uuid::Uuid;
use crate::{jsonrpc::{Id, Request, Response}, message::{ChannelCapabilities, ChannelState, HealthResult, InitParams, InitResult, ListKeysResult,}};

#[async_trait]
pub trait ControlClientType: Send + Sync + 'static {
    async fn name(&self) -> anyhow::Result<String>;
    async fn init(&self, params: InitParams) -> anyhow::Result<InitResult>;
    async fn start(&self, params: InitParams) -> anyhow::Result<InitResult>;
    async fn drain(&self) -> anyhow::Result<()>;
    async fn stop(&self) -> anyhow::Result<()>;
    async fn state(&self) -> anyhow::Result<ChannelState>;
    async fn health(&self) -> anyhow::Result<HealthResult>;
    async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities>;
    async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult>;
    async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult>;
    async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<()>;
    // Possibly: set_config, set_secrets, version, etc.
}
#[derive(Clone, Debug)]
pub enum ControlClient {
    Rpc(RpcControlClient),
    // Future: Grpc(GrpcControlClient), etc.
}


impl ControlClient {
    pub fn new(
        tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
    ) -> Self {
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

    async fn drain(&self) -> anyhow::Result<()> {
        match self {
            ControlClient::Rpc(client) => client.drain().await,
        }
    }

    async fn stop(&self) -> anyhow::Result<()> {
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

    async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<()> {
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
    pub fn new(
        tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
    ) -> Self {
        Self { tx }
    }

    /// Generic helper: send `method` with optional `params` and
    /// deserialize the JSON-RPC result into `R`.
    async fn call<R>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<R>
    where
        R: DeserializeOwned,
    {
        let id = Id::String(Uuid::new_v4().to_string());
        let req = Request::call(id.clone(), method.to_owned(), params);

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
        let val = rsp
            .result
            .ok_or_else(|| anyhow!("missing result field"))?;
        Ok(serde_json::from_value(val)?)
    }
}

#[async_trait]
impl ControlClientType for RpcControlClient {
    async fn name(&self) -> anyhow::Result<String> {
        let name: String = self.call(&"name",None).await.expect("Cannot retrieve name");
        return Ok(name);
    }

    async fn init(&self, p: InitParams) -> Result<InitResult> {
        self.call("init", Some(serde_json::to_value(p)?)).await
    }

    async fn start(&self, p: InitParams) -> Result<InitResult> {
        self.call("start", Some(serde_json::to_value(p)?)).await
    }

    async fn drain(&self) -> Result<()> {
        self.call::<()>(&"drain", None).await
    }

    async fn stop(&self) -> Result<()> {
        self.call::<()>(&"stop", None).await
    }

    async fn state(&self) -> Result<ChannelState> {
        self.call("state", None).await
    }

    async fn health(&self) -> Result<HealthResult> {
        self.call("health", None).await
    }

    async fn capabilities(&self) -> Result<ChannelCapabilities> {
        self.call("capabilities", None).await
    }

    async fn list_config_keys(&self) -> Result<ListKeysResult> {
        self.call("listConfigKeys", None).await
    }

    async fn list_secret_keys(&self) -> Result<ListKeysResult> {
        self.call("listSecretKeys", None).await
    }

    async fn wait_until_drained(&self, t_ms: u64) -> Result<()> {
        self.call::<()>(
            "waitUntilDrained",
            Some(serde_json::json!({ "timeout_ms": t_ms })),
        )
        .await
    }
}
