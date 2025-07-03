use std::{fmt, sync::Arc};
use channel_plugin::{channel_client::{ChannelClient, ChannelClientType}, control_client::{ControlClient, ControlClientType}, message::{ChannelCapabilities, ChannelMessage, ChannelState, InitParams, ListKeysResult,}, plugin_actor::PluginHandle, plugin_helpers::PluginError, plugin_runtime::VERSION};
use crossbeam_utils::atomic::AtomicCell;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::json;
use crate::{flow::session::SessionStore, logger::LogConfig}; 

pub type Plugin = (ChannelClient,ControlClient);
pub struct PluginWrapper {
    name: String,
    msg: ChannelClient,
    inner: ControlClient,
    capabilities: ChannelCapabilities,
    config_keys: ListKeysResult,
    secret_keys: ListKeysResult,
    state:   Arc<AtomicCell<ChannelState>>,
    log_config: LogConfig,
    session_store: SessionStore,
}

impl Clone for PluginWrapper {
    fn clone(&self) -> Self {
        PluginWrapper {
            name: self.name.clone(),
            inner: self.inner.clone(),
            msg: self.msg.clone(),
            capabilities: self.capabilities.clone(),
            config_keys: self.config_keys.clone(),
            secret_keys: self.secret_keys.clone(),
            state: self.state.clone(),
            log_config: self.log_config.clone(),
            session_store: self.session_store.clone(),
        }
    }
}

impl fmt::Debug for PluginWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginWrapper")
            .field("name", &self.name)
            .field("state", &self.state)
            .field("log_config", &self.log_config)
            .field("session_store", &"<session_store>")
            .finish()
    }
}



impl PluginWrapper {
    pub async fn new(plugin: PluginHandle, session_store: SessionStore, log_config: LogConfig) -> Self {
        let inner = plugin.control_client();
        let msg = plugin.channel_client();
        let name = plugin.name();
        let capabilities = plugin.capabilities();
        let config_keys = plugin.list_config_keys();
        let secret_keys = plugin.list_secret_keys();
        Self {
            name,
            inner,
            msg,
            capabilities,
            config_keys,
            secret_keys,
            state:   Arc::new(AtomicCell::new(ChannelState::RUNNING)),
            log_config,
            session_store,
        }
    }

    pub fn session_store(&self) -> SessionStore {
        self.session_store.clone()
    }
  

    pub async fn schema_json(&self) -> anyhow::Result<(String, String)> {
        // 1) Manually generate the schema
        let mut generate = SchemaGenerator::default();
        let schema: Schema = <ChannelCapabilities>::json_schema(&mut generate);

        // 2) Fetch the real capabilities
        let name = self.inner.name().await.unwrap();
        let default_value = json!(self.capabilities().await);

        // 3) Inject the default into the metadata
        let mut schema_value = serde_json::to_value(&schema)?;
        if let Some(obj) = schema_value.as_object_mut() {
            obj.entry("default").or_insert(default_value);
        }

        // 4) Pretty print
        let text = serde_json::to_string_pretty(&schema_value)?;
        Ok((name, text))
    }

    pub fn name(&self) -> String {
        self.name.clone()
    }

    pub async fn capabilities(&self) -> ChannelCapabilities {
        self.capabilities.clone()
    }

    pub async fn list_config_keys(&self) -> ListKeysResult {
        self.config_keys.clone()
    }

    pub async  fn list_secret_keys(&self) -> ListKeysResult {
        self.secret_keys.clone()
    }

    pub async fn state(&self) -> ChannelState {
        self.inner.state().await.expect("Could not get state")
    }

    pub async fn start(&mut self, config: Vec<(String,String)>, secrets: Vec<(String,String)>) -> Result<(),PluginError> {
         let init = InitParams{ 
                version: VERSION.to_string(), 
                config: config, 
                secrets: secrets, 
                log_level: self.log_config.log_level.clone(), 
                log_dir: self.log_config.log_dir.clone(), 
                otel_endpoint: self.log_config.otel_endpoint.clone(), 
         };
        if self.inner.start(init).await.is_ok() {
            Ok(())
        } else {
            Err(PluginError::Other("start failed".into()))
        }
        
    }

    pub async fn drain(&mut self) -> Result<(),PluginError> {
        if self.inner.drain().await.is_ok() {
            self.state.store(ChannelState::DRAINING);
            Ok(())
        } else {
            Err(PluginError::Other("drain failed".into()))
        }
    }

    pub async fn wait_until_drained(&mut self, timeout_ms: u64) -> Result<(), PluginError> {
        if  self.inner.wait_until_drained(timeout_ms).await.is_ok() {
            Ok(())
        } else {
            Err(PluginError::Other("plugin_drain failed".into()))
        }
    }

    pub async fn stop(&mut self) -> Result<(),PluginError>{
        if self.inner.stop().await.is_ok()  {
            Ok(())
        } else {
            Err(PluginError::Other("stop failed".into()))
        }
    }

    
    #[tracing::instrument(name = "channel_send_message_async", skip(self, msg))]
    pub async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(), PluginError> {
        if self.msg.send(msg).await.is_ok() {
            Ok(())
        } else {
            Err(PluginError::Other("plugin_send_message returned false".into()))
        }
    }

    #[tracing::instrument(name = "channel_receive_message_async", skip(self))]
    pub async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage, PluginError> {
        match self.msg.next_inbound().await{
            Some(msg)=>Ok(msg),
            None =>Err(PluginError::Other("No message came back from next_inbound".to_string())),
        }
    }
    
}

#[cfg(test)]
pub mod tests {

    use crate::flow::session::InMemorySessionStore;

    use super::*;
    use channel_plugin::message::{ChannelMessage,};
    use channel_plugin::plugin_runtime::HasStore;
    use channel_plugin::plugin_test_util::spawn_mock_handle;

    pub async fn make_wrapper() -> PluginWrapper {
        let (_mock,plugin) =spawn_mock_handle().await;   //   👈 real PluginHandle!

        let store = InMemorySessionStore::new(60);
        PluginWrapper::new(plugin, store, LogConfig::default()).await
    }



    #[tokio::test]
    async fn test_send_and_poll() {
        let (mock,plugin_handle) = spawn_mock_handle().await;
        let store = InMemorySessionStore::new(60);
        let mut w = PluginWrapper::new(plugin_handle, store, LogConfig::default()).await;
        let config = vec![];
        let secrets = vec![];
        w.start(config,secrets).await.expect("could not start");
        let msg = ChannelMessage { id: "1".into(), ..Default::default() };
        assert!(w.send_message(msg.clone()).await.is_ok());
        mock.inject(msg.clone()).await;
        let got = w.receive_message().await.unwrap();
        assert_eq!(got.id, "1");
    }

    #[tokio::test]
    async fn test_capabilities_and_state() {
        let mut w = make_wrapper().await;
        assert_eq!(w.state().await, ChannelState::STOPPED);
        let config = vec![];
        let secrets = vec![];
        w.start(config, secrets).await.expect("could not start");
        assert_eq!(w.state().await, ChannelState::RUNNING);
        let caps = w.capabilities().await;
        assert_eq!(caps.name, "mock");
    }

    #[tokio::test]
    async fn test_lifecycle_methods() {
        let mut w = make_wrapper().await;
        let config = vec![];
        let secrets = vec![];   
        w.start(config, secrets).await.expect("could not start");
        let _ = w.drain().await;
        assert_eq!(w.state().await, ChannelState::DRAINING);
        w.wait_until_drained(10).await.expect("could not drain 2");
        w.stop().await.expect("could not stop");
        assert_eq!(w.state().await, ChannelState::STOPPED);
    }

    #[tokio::test]
    async fn test_config_and_secrets() {
        let (mock,plugin_handle) =spawn_mock_handle().await;
        let store = InMemorySessionStore::new(60);
        let mut w = PluginWrapper::new(plugin_handle, store, LogConfig::default()).await;
        let config = vec![("k".to_string(),"v".to_string())];
        let secrets = vec![("k".to_string(),"s".to_string())];
        w.start(config, secrets).await.expect("could not start");
        let config_store = mock.config_store();
        let secret_store = mock.secret_store();
        let config_v = config_store.get("k").expect("value not found").to_string();
        let secret_s = secret_store.get("k").expect("secret not found").to_string();
        assert_eq!(config_v,"v".to_string());
        assert_eq!(secret_s, "s".to_string());
        
    }

}