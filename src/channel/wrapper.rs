use std::{ffi::{c_char, CStr, CString}, sync::Arc};
use dashmap::DashMap;
use channel_plugin::{message::{ChannelCapabilities, ChannelMessage}, plugin::{ChannelPlugin, ChannelState, PluginError, PluginLogger}};
use crossbeam_utils::atomic::AtomicCell;
use schemars::{schema::{Metadata}, schema_for};
use serde_json::json;
 use async_trait::async_trait;
use super::plugin::Plugin;
use async_ffi::FfiFuture;          // for the FFI‐safe future

#[derive(Clone,Debug)]
pub struct PluginWrapper {
    inner: Arc<Plugin>,
    state:   Arc<AtomicCell<ChannelState>>,
    logger: Option<PluginLogger>,
}

impl PluginWrapper {
    pub fn new(inner: Arc<Plugin>) -> Self {
        Self {
            inner,
            state:   Arc::new(AtomicCell::new(ChannelState::Running)),
            logger: None,
        }
    }

    /// Fetch the plugin’s capabilities via FFI, or fall back to default.
    pub fn channel_capabilities(&self) -> ChannelCapabilities {
        // 1) Prepare an un‐initialized (well, default) struct on the stack.
        let mut caps = ChannelCapabilities::default();
        // 2) Call the FFI entry‐point.  It will write into `caps` if it returns `true`.
        let ok = unsafe {
            (self.inner.caps)(self.inner.handle, &mut caps as *mut _)
        };
        // 3) If the call succeeded, return what it wrote; otherwise return a safe default.
        if ok {
            caps
        } else {
            ChannelCapabilities::default()
        }
    }
    /// Return a JSON‐Schema for `ChannelCapabilities`, with
    /// the plugin’s real capabilities baked in as the schema’s `default`.
    pub fn schema_json(&self) -> anyhow::Result<(String,String)> {
        // 1) Generate the stock JSON‐Schema for the type.
        let mut root_schema = schema_for!(ChannelCapabilities);

        // 2) Fetch the real capabilities from the plugin
        let caps = self.channel_capabilities();

        // 3) Stuff them into the schema's metadata.default field
        let meta: &mut Metadata = root_schema
            .schema
            .metadata
            .get_or_insert_with(Default::default);
        meta.default = Some(json!(caps));

        // 4) And now pretty‐print that enriched schema
        let text = serde_json::to_string_pretty(&root_schema)?;
        Ok((caps.name,text))
    }
}
#[async_trait]
impl ChannelPlugin for PluginWrapper {
    fn name(&self) -> String {
        // 1) call the FFI, get a *mut c_char
        let ptr = unsafe { (self.inner.name)(self.inner.handle) };
        if ptr.is_null() {
            return String::new();
        }

        // 2) read it into a Rust String
        let name = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();

        // 3) free the C buffer
        unsafe { (self.inner.free_string)(ptr) };

        name
    }

    fn capabilities(&self) -> ChannelCapabilities {
        let mut caps = ChannelCapabilities::default();
        let ok = unsafe { (self.inner.caps)(self.inner.handle, &mut caps as *mut _) };
        if ok { caps } else { ChannelCapabilities::default() }
    }

    fn set_config(&mut self, config: DashMap<String, String>) {
        let json = serde_json::to_string(&config).unwrap_or_default();
        let c = CString::new(json).unwrap();
        unsafe { (self.inner.set_config)(self.inner.handle, c.as_ptr()) };
    }

    fn set_secrets(&mut self, secrets: DashMap<String, String>) {
        let json = serde_json::to_string(&secrets).unwrap_or_default();
        let c = CString::new(json).unwrap();
        unsafe { (self.inner.set_secrets)(self.inner.handle, c.as_ptr()) };
    }

    fn list_config(&self) -> Vec<String> {
        // 1) call the FFI, get a *mut c_char
        let ptr = unsafe { (self.inner.list_config)(self.inner.handle) };
        if ptr.is_null() {
            return Vec::new();
        }

        // 2) read it into a Rust String
        let json = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();

        // 3) free the C buffer
        unsafe { (self.inner.free_string)(ptr) };

        // 4) parse the JSON array, or default empty
        serde_json::from_str(&json).unwrap_or_default()
    }

    fn list_secrets(&self) -> Vec<String> {
        let ptr = unsafe { (self.inner.list_secrets)(self.inner.handle) };
        if ptr.is_null() {
            return Vec::new();
        }
        let json = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { (self.inner.free_string)(ptr) };
        serde_json::from_str(&json).unwrap_or_default()
    }

    fn state(&self) -> ChannelState {
        // just call the FFI, get back the enum by value:
        unsafe { (self.inner.state)(self.inner.handle) }
    }

    async fn start(&mut self) -> Result<(),PluginError> {
        let fut: FfiFuture<bool> = unsafe {
            (self.inner.start)(self.inner.handle)
        };
        // Await the FFI future
        let ok = fut.await;
        if ok {
            Ok(())
        } else {
            Err(PluginError::Other("start failed".into()))
        }
        
    }

    fn drain(&mut self) -> Result<(),PluginError> {
        if unsafe { (self.inner.drain)(self.inner.handle) } {
            self.state.store(ChannelState::Draining);
            Ok(())
        } else {
            Err(PluginError::Other("drain failed".into()))
        }
    }

    async fn wait_until_drained(&mut self, timeout_ms: u64) -> Result<(), PluginError> {
        let fut: FfiFuture<bool> = unsafe {
             (self.inner.wait_until_drained)(self.inner.handle, timeout_ms)
        };
        // Await the FFI future
        let ok = fut.await;
        if ok {
            Ok(())
        } else {
            Err(PluginError::Other("plugin_drain failed".into()))
        }
    }

    async fn stop(&mut self) -> Result<(),PluginError>{
        let fut: FfiFuture<bool> = unsafe {
            (self.inner.stop)(self.inner.handle)
        };
        // Await the FFI future
        let ok = fut.await;
        if ok {
            Ok(())
        } else {
            Err(PluginError::Other("stop failed".into()))
        }
    }
    
    fn set_logger(&mut self, logger: channel_plugin::plugin::PluginLogger) {
            // call into the plugin’s FFI entry‐point:
            unsafe {
                (self.inner.set_logger)(self.inner.handle, logger);
            }
    }

    fn get_logger(&self) -> Option<channel_plugin::plugin::PluginLogger> {
        self.logger
    }
    
    #[tracing::instrument(name = "channel_send_message_async", skip(self, msg))]
    async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(), PluginError> {
        // Call the FFI shim, passing in the raw pointer to our C‐allocated message
        let fut: FfiFuture<bool> = unsafe {
            (self.inner.send_message)(self.inner.handle, &msg as *const _)
        };
        // Await the FFI future
        let ok = fut.await;
        if ok {
            Ok(())
        } else {
            Err(PluginError::Other("plugin_send_message returned false".into()))
        }
    }

    #[tracing::instrument(name = "channel_receive_message_async", skip(self))]
    async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage, PluginError> {
        // Call the FFI shim that returns an FfiFuture<ChannelMessage>
        let fut: FfiFuture<*mut c_char> = unsafe {
            (self.inner.receive_message)(self.inner.handle)
        };
        // 2) Await it
        println!("@@@ REMOVE await fut before");
        let ptr = fut.await;
        println!("@@@ REMOVE await fut after");
        if ptr.is_null() {
            return Err(PluginError::Other("receive_message returned null".into()).into());
        }
        // 3) Convert to Rust String
        let cstr = unsafe { CStr::from_ptr(ptr) };
        let json = cstr.to_string_lossy().into_owned();

        // 4) Free the C‐allocated string
        unsafe { (self.inner.free_string)(ptr) };

        // 5) Deserialize into ChannelMessage
        let msg: ChannelMessage = serde_json::from_str(&json)
            .map_err(|e| PluginError::Other(format!("JSON parse error: {}", e)))?;

        Ok(msg)
    }
    
}

#[cfg(test)]
pub mod tests {

    use super::*;
    use async_ffi::{BorrowingFfiFuture, FutureExt};
    use channel_plugin::message::{ChannelMessage, ChannelCapabilities};
    use channel_plugin::plugin::{ChannelState, LogLevel, PluginLogger};
    use channel_plugin::PluginHandle;
    use dashmap::DashMap;
    use std::ffi::c_void;
    use std::os::raw::c_char;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    pub struct FakePlugin {
        sent: Mutex<Vec<ChannelMessage>>,
        polled: Mutex<Vec<ChannelMessage>>,
        state: Mutex<ChannelState>,
        caps: ChannelCapabilities,
        send_ok: bool,
        logger: Mutex<Option<PluginLogger>>,
    }

    impl FakePlugin {
        pub fn new() -> Arc<Self> {
            Arc::new(FakePlugin {
                sent: Mutex::new(vec![]),
                polled: Mutex::new(vec![]),
                state: Mutex::new(ChannelState::Stopped),
                caps: ChannelCapabilities { name: "Fake".into(), ..Default::default() },
                send_ok: true,
                logger: Mutex::new(None),
            })
        }


        // new “set_logger” FFI entry‐point:
        unsafe extern "C" fn set_logger_fn(
            handle: PluginHandle,
            logger: PluginLogger,
        ) {
            let plugin = unsafe { &*(handle as *const FakePlugin) };
            let mut slot = plugin.logger.lock().unwrap();
            *slot = Some(logger);
        }
        // helper for tests to invoke the logger:
        fn call_logged(&self, level: LogLevel, ctx: &str, msg: &str) {
            if let Some(logger) = &*self.logger.lock().unwrap() {
                // this just calls the `PluginLogger::log` shim
                logger.log(level, ctx, msg);
            }
        }

        unsafe extern "C" fn caps_fn(handle: PluginHandle, out: *mut ChannelCapabilities) -> bool {
            let plugin = unsafe { &*(handle as *const FakePlugin) };
            unsafe { std::ptr::write(out, plugin.caps.clone()) };
            true
        }

        unsafe extern "C" fn state_fn(handle: PluginHandle) -> ChannelState {
            // Cast to a *const FakePlugin, then borrow without moving
            let plugin = unsafe { &*(handle as *const FakePlugin) };
            plugin.state.lock().unwrap().clone()
        }

        unsafe extern "C" fn start_fn(handle: PluginHandle) -> FfiFuture<bool> {
            // Cast to *mut FakePlugin so we can mutate through the Mutex
            let plugin = unsafe { &*(handle as *const FakePlugin) };
            *plugin.state.lock().unwrap() = ChannelState::Running;
            return BorrowingFfiFuture::<bool>::new(async move {true}); 
        }

        unsafe extern "C" fn stop_fn(handle: PluginHandle) -> FfiFuture<bool> {
            // Cast to *mut FakePlugin so we can mutate through the Mutex
            let plugin = unsafe { &*(handle as *const FakePlugin) };
            *plugin.state.lock().unwrap() = ChannelState::Stopped;
            return BorrowingFfiFuture::<bool>::new(async move {true}); 
        }
        unsafe extern "C" fn drain_fn(handle: PluginHandle) -> bool {
            let plugin = unsafe { &*(handle as *const FakePlugin) };
            *plugin.state.lock().unwrap() = ChannelState::Draining;
            true
        }
        unsafe extern "C" fn wait_fn(_: PluginHandle, _: u64) -> FfiFuture<bool> 
        { return BorrowingFfiFuture::<bool>::new(async move {true}); }
        unsafe extern "C" fn set_config_fn(_: PluginHandle, _: *const c_char) {}
        unsafe extern "C" fn set_secrets_fn(_: PluginHandle, _: *const c_char) {}
        
        unsafe extern "C" fn name_fn(_: PluginHandle) -> *mut c_char {
            // static NUL-terminated C string; we cast to *mut even though data is read-only
            static NAME: &[u8] = b"name\0";
            NAME.as_ptr() as *mut c_char
        }
        unsafe extern "C" fn list_config_fn(_: PluginHandle) -> *mut c_char { std::ptr::null_mut() }
        unsafe extern "C" fn list_secrets_fn(_: PluginHandle) -> *mut c_char { std::ptr::null_mut() }
        unsafe extern "C" fn free_string_fn(_: *mut c_char) {}

        // 1) Async‐style send → FfiFuture<bool>
        unsafe extern "C" fn send_message_fn(
            handle: PluginHandle,
            msg: *const ChannelMessage,
        ) -> FfiFuture<bool> {
            let plugin = unsafe { &*(handle as *const FakePlugin) };
            let msg = unsafe { &*msg };
            plugin.sent.lock().unwrap().push(msg.clone());
            BorrowingFfiFuture::<bool>::new(async move {plugin.send_ok})
        }

        // 2) Async‐style receive → FfiFuture<ChannelMessage>
        unsafe extern "C" fn receive_message_fn(
            handle: PluginHandle,
        ) -> BorrowingFfiFuture<'static, *mut c_char> {
            let plugin = unsafe { &*(handle as *const FakePlugin) };
            let fut = async move {
                let msg = plugin.polled.lock().unwrap().pop();
                
                // serialize to JSON
                let js = serde_json::to_string(&msg).unwrap_or_default();
                CString::new(js).unwrap().into_raw()
            };
            fut.into_ffi()
        }

        unsafe extern "C" fn destroy(_: PluginHandle) {}
    }

    pub fn make_wrapper() -> PluginWrapper {
        let fake = FakePlugin::new();
        let p = Plugin {
            lib: None,
            handle: Arc::into_raw(fake.clone()) as PluginHandle,
            destroy: FakePlugin::destroy,
            set_logger: FakePlugin::set_logger_fn,
            name: FakePlugin::name_fn,
            start: FakePlugin::start_fn,
            drain: FakePlugin::drain_fn,
            stop: FakePlugin::stop_fn,
            wait_until_drained: FakePlugin::wait_fn,
            caps: FakePlugin::caps_fn,
            state: FakePlugin::state_fn,
            set_config:  FakePlugin::set_config_fn,
            set_secrets: FakePlugin::set_secrets_fn,
            list_config: FakePlugin::list_config_fn,
            list_secrets:FakePlugin::list_secrets_fn,
            send_message: FakePlugin::send_message_fn,
            receive_message: FakePlugin::receive_message_fn,
            free_string: FakePlugin::free_string_fn,
            last_modified: SystemTime::now(),
            path: PathBuf::new(),
        };
        PluginWrapper::new(Arc::new(p))
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

    #[test]
    fn test_set_logger_and_callback() {
        // 1) make our wrapper
        let mut wrapper = make_wrapper();
        let (test_logger, calls) = TestLogger::new();
            // b) Box it up and turn it into a raw pointer
        let boxed = Box::new(test_logger);
        let ctx = Box::into_raw(boxed) as *mut c_void;

        // c) Build the FFI‐struct
        let ffi_logger = PluginLogger {
            ctx,
            log_fn: test_log_fn,
        };

        // 2) build a TestLogger and install it
        wrapper.set_logger(ffi_logger);

        // 3) now simulate the plugin “logging” something by calling into our helper
        let fake = unsafe { &*(wrapper.inner.handle as *const FakePlugin) };
        fake.call_logged(LogLevel::Warn, "fake-ctx", "oh no!");

        // 4) verify our TestLogger saw it
        let locked = calls.lock().unwrap();
        assert_eq!(
            locked.as_slice(),
            &[(LogLevel::Warn, "fake-ctx".into(), "oh no!".into())]
        );
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
}