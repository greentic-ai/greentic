use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::{jsonrpc::{Id, Request}, message::{ChannelCapabilities, ChannelState, HealthResult, InitParams, InitResult, ListKeysResult, WaitUntilDrainedParams}, plugin_actor::{Command, Method}};

#[async_trait]
pub trait ControlClient: Send + Sync + 'static {
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

#[derive(Clone)]
pub struct InProcControlClient {
    cmd_tx: mpsc::Sender<Command>,
}

impl InProcControlClient{
    pub fn new(cmd_tx: mpsc::Sender<Command>) -> Self {
        Self{cmd_tx}
    }
}


#[async_trait]
impl ControlClient for InProcControlClient {
    async fn init(&self, params: InitParams) -> anyhow::Result<InitResult> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(Command::Start(params, tx)).await?;  // using Start for both init/start
        rx.await.map_err(|e| e.into())
    }
    async fn start(&self, params: InitParams) -> anyhow::Result<InitResult> {
        self.init(params).await  // In this design, start may alias init.
    }
    async fn drain(&self) -> anyhow::Result<()> {
        self.cmd_tx.send(Command::Drain).await?;
        Ok(())
    }
    async fn stop(&self) -> anyhow::Result<()> {
        self.cmd_tx.send(Command::Stop).await?;
        Ok(())
    }
    async fn state(&self) -> anyhow::Result<ChannelState> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(Command::State(tx)).await?;
        Ok(rx.await?.state)
    }
    async fn health(&self) -> anyhow::Result<HealthResult> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(Command::Health(tx)).await?;
        rx.await.map_err(|e| e.into())
    }
    async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(Command::Capabilities(tx)).await?;
        Ok(rx.await?.capabilities)
    }
    async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult> {
        let (tx, rx) = oneshot::channel();
        let req = Request::call(Id::Null, Method::ListConfigKeys.as_ref().to_string(), None);
        self.cmd_tx.send(Command::Call(req, tx)).await?;
        let resp = rx.await?;
        // Deserialize the JSON result into ListKeysResult
        let result_val = resp.result.ok_or_else(|| anyhow::anyhow!("Missing result"))?;
        Ok(serde_json::from_value(result_val)?)
    }
    async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult> {
        // Similar to list_config_keys
        let (tx, rx) = oneshot::channel();
        let req = Request::call(Id::Null, Method::ListSecretKeys.as_ref().to_string(), None);
        self.cmd_tx.send(Command::Call(req, tx)).await?;
        let resp = rx.await?;
        let result_val = resp.result.ok_or_else(|| anyhow::anyhow!("Missing result"))?;
        Ok(serde_json::from_value(result_val)?)
    }
    async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(Command::WaitUntilDrained(WaitUntilDrainedParams { timeout_ms }, tx)).await?;
        let res = rx.await?;
        if res.error { 
            Err(anyhow::anyhow!("Timed out before drained")) 
        } else { 
            Ok(()) 
        }
    }
}