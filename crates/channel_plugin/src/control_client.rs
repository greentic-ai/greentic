use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::sync::{mpsc, oneshot};
use anyhow::{anyhow, Result};
use uuid::Uuid;
use crate::{jsonrpc::{Id, Request, Response}, message::{ChannelCapabilities, ChannelState, HealthResult, InitParams, InitResult, ListKeysResult, WaitUntilDrainedParams}, plugin_actor::{Command, Method}};

#[async_trait]
pub trait ControlClient: Send + Sync + 'static {
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


pub trait CloneableControlClient: ControlClient {
    fn clone_box(&self) -> Box<dyn CloneableControlClient>;
}

impl<T> CloneableControlClient for T
where
    T: ControlClient + Clone + Send + 'static,
{
    fn clone_box(&self) -> Box<dyn CloneableControlClient> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn CloneableControlClient> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[async_trait::async_trait]
impl ControlClient for Box<dyn CloneableControlClient> {
    async fn name(&self) -> anyhow::Result<String> {
        (**self).name().await
    }
    async fn init(&self, p: InitParams) -> anyhow::Result<InitResult> {
        (**self).init(p).await
    }
    async fn start(&self, p: InitParams) -> anyhow::Result<InitResult> {
        (**self).start(p).await
    }
    async fn drain(&self) -> anyhow::Result<()> {
        (**self).drain().await
    }
    async fn stop(&self) -> anyhow::Result<()> {
        (**self).stop().await
    }
    async fn state(&self) -> anyhow::Result<ChannelState> {
        (**self).state().await
    }
    async fn health(&self) -> anyhow::Result<HealthResult> {
        (**self).health().await
    }
    async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
        (**self).capabilities().await
    }
    async fn list_config_keys(&self) -> anyhow::Result<ListKeysResult> {
        (**self).list_config_keys().await
    }
    async fn list_secret_keys(&self) -> anyhow::Result<ListKeysResult> {
        (**self).list_secret_keys().await
    }
    async fn wait_until_drained(&self, timeout_ms: u64) -> anyhow::Result<()> {
        (**self).wait_until_drained(timeout_ms).await
    }
}

#[derive(Debug)]
pub struct RpcControlClient {
    /// outbound <Request, responder> channel shared with the actor task
    tx: mpsc::Sender<(Request, oneshot::Sender<Response>)>,
}

impl CloneableControlClient for RpcControlClient{
    fn clone_box(&self) -> Box<dyn CloneableControlClient> {
        Box::new(Self {
            tx: self.tx.clone(),
        })
    }
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
impl ControlClient for RpcControlClient {
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



pub struct InProcControlClient {
    cmd_tx: mpsc::Sender<Command>,
}

impl InProcControlClient{
    pub fn new(cmd_tx: mpsc::Sender<Command>) -> Self {
        Self{cmd_tx}
    }
}

#[cfg(feature = "test-utils")]
impl CloneableControlClient for InProcControlClient {

    
    fn clone_box(&self) -> Box<dyn CloneableControlClient> {
        Box::new(Self {
            cmd_tx: self.cmd_tx.clone(),
        })
    }
}

#[async_trait]
impl ControlClient for InProcControlClient {
    async fn name(&self) -> anyhow::Result<String> {
        Ok(self.name().await.unwrap())  // In this design, start may alias init.
    }
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