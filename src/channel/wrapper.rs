use std::sync::Arc;
use channel_plugin::{message::{ChannelCapabilities, ChannelMessage, ChannelState, InitParams, ListKeysResult, LogLevel, MessageOutParams}, plugin_actor::PluginHandle, plugin_helpers::PluginError, plugin_manager::PluginManager, plugin_runtime::{PluginHandler, VERSION}};
use crossbeam_utils::atomic::AtomicCell;
use schemars::{schema_for, JsonSchema, Schema, SchemaGenerator};
use serde_json::json;
use crate::{flow::session::SessionStore, logger::LogConfig}; 


#[derive(Clone,Debug)]
pub struct PluginWrapper {
    inner: PluginHandle,
    state:   Arc<AtomicCell<ChannelState>>,
    log_config: LogConfig,
    session_store: SessionStore,
}

impl PluginWrapper {
    pub fn new(inner: PluginHandle, session_store: SessionStore, log_config: LogConfig) -> Self {
        Self {
            inner,
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
        let name = self.inner.id().to_string();
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
        self.inner.id().to_string()
    }

    pub async fn capabilities(&self) -> ChannelCapabilities {
        self.inner.capabilities().await.expect("Could not get capabilities")
    }

    pub async fn list_config_keys(&self) -> ListKeysResult {
        self.inner.list_config_keys().await.expect("Could not get config keys")
    }

    pub async  fn list_secret_keys(&self) -> ListKeysResult {
        self.inner.list_secret_keys().await.expect("Could not get config keys")
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
        if self.inner.send_message(MessageOutParams{message:msg}).await.is_ok() {
            Ok(())
        } else {
            Err(PluginError::Other("plugin_send_message returned false".into()))
        }
    }

    #[tracing::instrument(name = "channel_receive_message_async", skip(self))]
    pub async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage, PluginError> {
        match self.inner.receive_message().await{
            Ok(msg) => Ok(msg.message),
            Err(err) => Err(PluginError::Other(err.to_string())),
        }
    }
    
}

#[cfg(test)]
pub mod tests {

    use crate::flow::session::InMemorySessionStore;

    use super::*;
    use channel_plugin::message::ChannelMessage;
    use channel_plugin::plugin_actor::tests::make_mock_handle;
    use dashmap::{DashMap};
    use std::ffi::c_void;
    use std::os::raw::c_char;
    use std::sync::{Arc, Mutex};


    pub async fn make_wrapper() -> PluginWrapper {
        let plugin =make_mock_handle().await;   //   👈 real PluginHandle!

        let store = InMemorySessionStore::new(60);
        PluginWrapper::new(plugin, store, LogConfig::default())
    }


    /// A tiny in‐process logger we can inspect.
    struct TestLogger {
        calls: Arc<Mutex<Vec<(LogLevel, String, String)>>>,
    }

    impl TestLogger {
        fn new() -> (Self, Arc<Mutex<Vec<(LogLevel, String, String)>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (TestLogger { calls: calls.clone() }, calls)
        }

        fn record(&self, level: LogLevel, ctx: &str, msg: &str) {
            self.calls
                .lock()
                .unwrap()
                .push((level, ctx.to_string(), msg.to_string()));
        }
    }


    // 2) Your extern "C" callback that unpacks the FFI handle
    extern "C" fn test_log_fn(
        ctx: *mut c_void,
        level: LogLevel,
        context: *const c_char,
        message: *const c_char,
    ) {
        let logger = unsafe { &*(ctx as *const TestLogger) };
        let ctx_str = unsafe { CStr::from_ptr(context) }.to_string_lossy();
        let msg_str = unsafe { CStr::from_ptr(message) }.to_string_lossy();
        logger.record(level, &ctx_str, &msg_str);
    }


    #[tokio::test]
    async fn test_send_and_poll() {
        let mut w = make_wrapper();
        w.start().await.expect("could not start");
        let msg = ChannelMessage { id: "1".into(), ..Default::default() };
        assert!(w.send_message(msg.clone()).await.is_ok());
        let fake = unsafe { &*(w.inner.handle as *const FakePlugin) };
        fake.polled.lock().unwrap().push(msg.clone());
        let got = w.receive_message().await.unwrap();
        assert_eq!(got.id, "1");
    }

    #[tokio::test]
    async fn test_capabilities_and_state() {
        let mut w = make_wrapper();
        assert_eq!(w.state(), ChannelState::Stopped);
        w.start().await.expect("could not start");
        assert_eq!(w.state(), ChannelState::Running);
        let caps = w.capabilities();
        assert_eq!(caps.name, "Fake");
    }

    #[tokio::test]
    async fn test_lifecycle_methods() {
        let mut w = make_wrapper();
        w.start().await.expect("could not start");
        w.drain().expect("could not draing");
        assert_eq!(w.state(), ChannelState::Draining);
        w.wait_until_drained(10).await.expect("could not drain 2");
        w.stop().await.expect("could not stop");
        assert_eq!(w.state(), ChannelState::Stopped);
    }

    #[test]
    fn test_config_and_secrets() {
        let mut w = make_wrapper();
        let cfg = DashMap::new();
        cfg.insert("k".into(), "v".into());
        w.set_config(cfg);
        let sec = DashMap::new();
        sec.insert("s".into(), "t".into());
        w.set_secrets(sec);
    }

    #[tokio::test]
    async fn test_set_session_callbacks() {
        let wrapper = make_wrapper();
        let store = wrapper.session_store();

        // Initialize global SESSION_STORE for plugin callbacks
        let _ = SESSION_STORE.set(store.clone());

        // Manually invoke the session callback FFI registration (bypassing lib.get)
        if let Some(set_fns) = wrapper.inner.set_session_callbacks {
            unsafe { set_fns(wrapper.inner.handle, PluginSessionCallbacks {
                get_session: crate::channel::message::plugin_get_session,
                get_or_create_session: crate::channel::message::plugin_get_or_create_session,
                invalidate_session: crate::channel::message::plugin_invalidate_session,
            }) };
        }

        // Verify they were actually set in the plugin
        let fake = unsafe { &*(wrapper.inner.handle as *const FakePlugin) };
        let stored = fake.session_callbacks.lock().unwrap();
        let fns = stored.as_ref().expect("Session callbacks were not registered");

        // Prep test values
        let plugin_name = CString::new("test_plugin").unwrap();
        let key = CString::new("user_123").unwrap();

        // First call: should create the session
        let fut = (fns.get_or_create_session)(plugin_name.as_ptr(), key.as_ptr());
        let raw = fut.await;
        let session_id = unsafe { CStr::from_ptr(raw) }.to_string_lossy().to_string();
        assert!(!session_id.is_empty(), "Should have received a session ID");

        // Second call: get_session should return the same
        let fut2 = (fns.get_session)(plugin_name.as_ptr(), key.as_ptr());
        let raw2 = fut2.await;
        let fetched = unsafe { CStr::from_ptr(raw2) }.to_string_lossy().to_string();
        assert_eq!(session_id, fetched, "Session IDs should match");

        // Invalidate session
        let sid_cstr = CString::new(session_id.clone()).unwrap();
        let fut3 = (fns.invalidate_session)(sid_cstr.as_ptr());
        fut3.await;

        // get_session should now return null
        let fut4 = (fns.get_session)(plugin_name.as_ptr(), key.as_ptr());
        let raw4 = fut4.await;
        assert!(raw4.is_null(), "Session should have been invalidated");
    }
}