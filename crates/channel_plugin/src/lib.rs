pub mod plugin;
pub mod message;
use std::ffi::c_void;
pub type PluginHandle = *mut c_void;

#[macro_export]
macro_rules! export_plugin {
    ($ty:ty) => {
        use std::ffi::{c_void, CStr, CString};
        use std::os::raw::c_char;
        use std::ptr;
        use channel_plugin::PluginHandle;
        use async_ffi::{BorrowingFfiFuture, FfiFuture, FutureExt};
        use tokio::runtime::Builder;

        pub type GetSessionFn = extern "C" fn(*const c_char, *const c_char) -> FfiFuture<*mut c_char>;
        pub type GetOrCreateSessionFn = extern "C" fn(*const c_char, *const c_char) -> FfiFuture<*mut c_char>;
        pub type InvalidateSessionFn = extern "C" fn(*const c_char) -> FfiFuture<()>;
        static mut GET_SESSION_FN: Option<GetSessionFn> = None;
        static mut GET_OR_CREATE_FN: Option<GetOrCreateSessionFn> = None;
        static mut INVALIDATE_SESSION_FN: Option<InvalidateSessionFn> = None;

        pub struct SessionApi {
            pub get: GetSessionFn,
            pub get_or_create: GetOrCreateSessionFn,
            pub invalidate: InvalidateSessionFn,
        }

        impl SessionApi {                
            /// Try to get session by unique plugin identifier and and run route matching if no session is available
            pub async fn get_session_id(plugin: &str, key: &str) -> Option<String>{
                let plugin_c = CString::new(plugin).unwrap();
                let key_c = CString::new(key).unwrap();

                let ptr = unsafe {
                    GET_OR_CREATE_FN
                        .expect("❌ Session callback not registered")(plugin_c.as_ptr(), key_c.as_ptr())
                        .await
                };

                if ptr.is_null() {
                    Some("unknown".to_string())
                } else {
                    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
                    unsafe { plugin_free_string(ptr) };
                    Some(s)
                }
            }

            /// Try to get or create session by plugin name + key and rely on session routing
            pub async fn get_or_create_session_id(plugin: &str, key: &str) -> String {
                let plugin_c = CString::new(plugin).unwrap();
                let key_c = CString::new(key).unwrap();

                let ptr = unsafe {
                    GET_OR_CREATE_FN
                        .expect("❌ get_or_create_session callback not registered")(plugin_c.as_ptr(), key_c.as_ptr())
                        .await
                };

                if ptr.is_null() {
                    "unknown".to_string()
                } else {
                    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
                    unsafe { plugin_free_string(ptr) };
                    s
                }
            }
            /// Explicitly removes a session from the store.
            pub async fn invalidate_session_id(session_id: &str) {
                let id_c = CString::new(session_id).unwrap();

                unsafe {
                    INVALIDATE_SESSION_FN
                        .expect("❌ invalidate_session callback not registered")(id_c.as_ptr())
                        .await;
                }
            }
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_create() -> $crate::PluginHandle {
            unsafe{
                let boxed: Box<$ty> = Box::new(<$ty>::default());
                Box::into_raw(boxed) as $crate::PluginHandle
            }
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_destroy(handle: $crate::PluginHandle) {
            if handle.is_null() { return; }
            // cast back to Box<$ty> and drop it
            let raw: *mut $ty = unsafe {handle as *mut $ty};
            unsafe{drop(Box::from_raw(raw))};
        }

        /// Plugin authors implement this in their `#[typetag]` impl:
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_set_logger(handle: $crate::PluginHandle, logger: PluginLogger) {
            if handle.is_null() {
                return;
            }
            // plugin is really a pointer to your concrete type
            let plugin = unsafe{&mut *(handle as *mut $ty) as &mut dyn ChannelPlugin};
            plugin.set_logger(logger);
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_name(
            handle: $crate::PluginHandle
        ) -> *mut c_char {
            if handle.is_null() {
                return std::ptr::null_mut();
            }
            let plugin = unsafe{&*(handle as *const $ty)};
            let name: String = unsafe{< $ty as $crate::plugin::ChannelPlugin >::name(plugin)};

            unsafe{CString::new(name).unwrap().into_raw()}
        }


        // 5) The `capabilities` wrapper:
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_capabilities(
            handle: $crate::PluginHandle,
            out: *mut $crate::message::ChannelCapabilities,
        ) -> bool {
            if handle.is_null() || out.is_null() { return false; }
            let plugin = unsafe{&*(handle as *const $ty)};
            let caps = unsafe{< $ty as $crate::plugin::ChannelPlugin >::capabilities(plugin)};
            unsafe{ptr::write(out, caps)};
            true
        }

        // 6) `config` and `secrets` → JSON strings:
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_set_config(
            handle: $crate::PluginHandle,
            json: *const c_char,
        ) {
            if handle.is_null() || json.is_null() { return; }
            let plugin = unsafe{&mut *(handle as *mut $ty)};
            let s = unsafe{CStr::from_ptr(json).to_string_lossy()};
            if let Ok(cfg) = serde_json::from_str(&s)
            {
               unsafe{< $ty as $crate::plugin::ChannelPlugin>::set_config(plugin, cfg)};
            }
        }

        /// NEW FFI: pass a JSON blob of the secrets map into the plugin
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_set_secrets(
            handle: $crate::PluginHandle,
            json: *const c_char,
        ) {
            if handle.is_null() || json.is_null() { return; }
            let plugin = unsafe{&mut *(handle as *mut $ty)};
            let s = unsafe{CStr::from_ptr(json).to_string_lossy()};
            if let Ok(sec) = serde_json::from_str(&s)
            {
                unsafe{< $ty as $crate::plugin::ChannelPlugin>::set_secrets(plugin, sec)};
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_free_string(s: *mut c_char) {
            if !s.is_null() {
                unsafe{drop(CString::from_raw(s))};
            }
        }

        // 6.1) List the config keys the plugin wants:
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_list_config(
            handle: $crate::PluginHandle
        ) -> *mut c_char {
            if handle.is_null() {
                return std::ptr::null_mut();
            }
            let plugin = unsafe{&*(handle as *const $ty)};
            let list: Vec<String> = unsafe{< $ty as $crate::plugin::ChannelPlugin >::list_config(plugin)};
            match serde_json::to_string(&list) {
                Ok(js) => {
                    // allocate a C string and return its pointer
                    unsafe{CString::new(js).unwrap().into_raw()}
                }
                Err(_) => std::ptr::null_mut(),
            }
        }

        // 6.2) List the secret keys the plugin wants:
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_list_secrets(
            handle: $crate::PluginHandle
        ) -> *mut c_char {
            if handle.is_null() {
                return std::ptr::null_mut();
            }
            let plugin = unsafe{&*(handle as *const $ty)};
            let list: Vec<String> = < $ty as $crate::plugin::ChannelPlugin >::list_secrets(plugin);
            match serde_json::to_string(&list) {
                Ok(js) => unsafe{CString::new(js).unwrap().into_raw()},
                Err(_) => std::ptr::null_mut(),
            }
        }


        // 8) `state`, `start`, `drain`, `wait_until_drained`, `stop`:
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_state(
            handle: $crate::PluginHandle,
        ) -> $crate::plugin::ChannelState {
            if handle.is_null() {
                return $crate::plugin::ChannelState::Stopped;
            }
            let plugin = unsafe{ &*(handle as *const $ty)};
            unsafe{< $ty as $crate::plugin::ChannelPlugin >::state(plugin)}
        }

       #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_start(handle: $crate::PluginHandle) ->  FfiFuture<bool> {
            if handle.is_null() {  return BorrowingFfiFuture::<bool>::new(async move {false}); }

            let raw = handle as usize;
            let logger: Option<PluginLogger> = {
                let plugin = unsafe{&*(handle as *mut $ty)};
                plugin.get_logger().clone()
            };
            let name = {
                let plugin = unsafe{&*(handle as *mut $ty)};
                plugin.name().clone()
            };
            // We immediately return “true” (or queued launch) to the caller,
            // while the plugin.start() actually runs on its own thread.
            BorrowingFfiFuture::<bool>::new(async move {
                
                // run your async start() under that runtime
                if tokio::runtime::Handle::try_current().is_ok() {
                    tokio::spawn(async move {
                        let plugin_ptr = raw as *mut $ty;
                        let plugin = unsafe {&mut *plugin_ptr };      // SAFETY: handle is valid
                        if let Err(e) = plugin.start().await {
                            // if we had a logger, emit an error
                            if let Some(lg) = &logger {
                                lg.log(
                                    LogLevel::Error,
                                    name.as_str(),
                                    &format!("start() failed: {:?}", e),
                                );
                            }
                        }
                    });
                } else {
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(async move {
                            let plugin_ptr = raw as *mut $ty;
                            let plugin = unsafe {&mut *plugin_ptr };      // SAFETY: handle is valid
                            if let Err(e) = plugin.start().await {
                                // if we had a logger, emit an error
                                if let Some(lg) = &logger {
                                    lg.log(
                                        LogLevel::Error,
                                        name.as_str(),
                                        &format!("start() failed: {:?}", e),
                                    );
                                }
                            }
                        });
                    });
                }


                // report “we kicked it off”
                true
            })
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_drain(handle: $crate::PluginHandle) -> bool {
            if handle.is_null() { return false; }
            let plugin = unsafe{&mut *(handle as *mut $ty)};
            < $ty as ChannelPlugin>::drain(plugin).is_ok()
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_wait_until_drained(
            handle: $crate::PluginHandle,
            timeout_ms: u64,
        ) -> FfiFuture<bool> {
            if handle.is_null() {  return BorrowingFfiFuture::<bool>::new(async move {false}); }
            let raw = handle as usize;
            BorrowingFfiFuture::<'static, bool>::new(async move {
                if raw == 0 {
                    return false;
                }
                let plugin = unsafe { &mut *(raw as *mut $ty) };
                match < $ty as ChannelPlugin>::wait_until_drained(plugin, timeout_ms).await {
                    Ok(()) => true,
                    Err(_) => false,
                }
            })
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_stop(handle: $crate::PluginHandle) -> FfiFuture<bool> {
            if handle.is_null() {  return BorrowingFfiFuture::<bool>::new(async move {false}); }
            let raw = handle as usize;
            BorrowingFfiFuture::<'static, bool>::new(async move {
                if raw == 0 {
                    return false;
                }
                let plugin = unsafe { &mut *(raw as *mut $ty) };
                match < $ty as ChannelPlugin>::stop(plugin).await {
                    Ok(()) => true,
                    Err(_) => false,
                }
            })
        }
        
        // async_ffi

        #[unsafe(no_mangle)]
        pub extern "C" fn plugin_send_message(
            handle: PluginHandle,
            msg: *const ChannelMessage,
        ) -> FfiFuture<bool> {
            // quick null checks
            if handle.is_null() || msg.is_null() {
                return BorrowingFfiFuture::<bool>::new(async move {false})
            }
            // recover Rust references
            let raw = handle as usize;
            let raw_msg    = msg as usize;
            BorrowingFfiFuture::<'static, bool>::new(async move {
                if raw == 0 || raw_msg == 0 {
                    return false;
                }
                let plugin = unsafe { &mut *(raw as *mut $ty) };
                let message: ChannelMessage = unsafe { &*(raw_msg as *const ChannelMessage) }.clone();
                match < $ty as $crate::plugin::ChannelPlugin >::send_message(plugin, message).await {
                    Ok(()) => true,
                    Err(_) => false,
                }
            })
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn plugin_receive_message(handle: PluginHandle) -> BorrowingFfiFuture<'static, *mut c_char> {
            if handle.is_null() {
                return BorrowingFfiFuture::<*mut c_char>::new(async move {std::ptr::null_mut()})
            }
            let raw = handle as usize;

            BorrowingFfiFuture::<'static, *mut c_char>::new(async move {
                if raw == 0 {
                    return std::ptr::null_mut();
                }
                let plugin = unsafe { &mut *(raw as *mut $ty) };
                let msg = unsafe{< $ty as ChannelPlugin >::receive_message(plugin).await
                    .unwrap_or_default()};
                // serialize to JSON
                let js = serde_json::to_string(&msg).unwrap_or_default();
                CString::new(js).unwrap().into_raw()
            })
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn greentic_register_session_fns(
            get_fn: GetSessionFn,
            get_or_create_fn: GetOrCreateSessionFn,
            invalidate_fn: InvalidateSessionFn,
        ) {
            println!("@@@ REMOVE PLUGIN RECEIVED SESSION FNS!!!!");
            // Store these in a static or thread-safe once cell for use by plugin
            unsafe {
                GET_SESSION_FN = Some(get_fn);
                GET_OR_CREATE_FN = Some(get_or_create_fn);
                INVALIDATE_SESSION_FN = Some(invalidate_fn);
            }
        }

        /// Get an existing session for a plugin-specific key
        #[unsafe(no_mangle)]
        pub extern "C" fn greentic_get_session(
            plugin_name: *const c_char,
            key: *const c_char,
        ) -> BorrowingFfiFuture<'static, *mut c_char> {
            if plugin_name.is_null() || key.is_null() {
                return BorrowingFfiFuture::new(async { std::ptr::null_mut() });
            }

            let plugin_name = unsafe { CStr::from_ptr(plugin_name).to_owned() };
            let key = unsafe { CStr::from_ptr(key).to_owned() };

            BorrowingFfiFuture::new(async move {
                if let Some(f) = unsafe { GET_SESSION_FN } {
                    // Safety: we convert back to raw pointer for the FFI function
                    f(plugin_name.as_ptr(), key.as_ptr()).await
                } else {
                    std::ptr::null_mut()
                }
            })
        }

        /// Get or create a session for a plugin-specific key
        #[unsafe(no_mangle)]
        pub extern "C" fn greentic_get_or_create_session(
            plugin_name: *const c_char,
            key: *const c_char,
        ) -> BorrowingFfiFuture<'static, *mut c_char> {
            println!("@@@ REMOVE GOT SESSION CALL");
            if plugin_name.is_null() || key.is_null() {
                return BorrowingFfiFuture::new(async { std::ptr::null_mut() });
            }
            println!("@@@ REMOVE FFI received request for plugin `{:?}` with key `{:?}`", plugin_name, key);

            let plugin_name = unsafe { CStr::from_ptr(plugin_name).to_owned() };
            let key = unsafe { CStr::from_ptr(key).to_owned() };

            BorrowingFfiFuture::new(async move {
                if let Some(f) = unsafe { GET_OR_CREATE_FN } {
                    let ptr = f(plugin_name.as_ptr(), key.as_ptr()).await;
                    if ptr.is_null() {
                        eprintln!("⚠️  GET_OR_CREATE FFI returned null for plugin: {:?}, key: {:?}", plugin_name, key);
                    }
                    ptr
                } else {
                    eprintln!("❌ GET_OR_CREATE session function was never registered!");
                    std::ptr::null_mut()
                }
            })
        }
        /// Remove a session by ID
        pub extern "C" fn greentic_invalidate_session(
            session_id: *const c_char,
        ) -> BorrowingFfiFuture<'static, ()> {
            if session_id.is_null() {
                return BorrowingFfiFuture::new(async {});
            }

            let session_id = unsafe { CStr::from_ptr(session_id).to_owned() };

            BorrowingFfiFuture::new(async move {
                if let Some(f) = unsafe {INVALIDATE_SESSION_FN} {
                    f(session_id.as_ptr()).await;
                }
            })
        }

    };
}