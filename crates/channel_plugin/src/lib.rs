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


        #[no_mangle]
        pub unsafe extern "C" fn plugin_create() -> $crate::PluginHandle {
            let boxed: Box<$ty> = Box::new(<$ty>::default());
            Box::into_raw(boxed) as $crate::PluginHandle
        }

        #[no_mangle]
        pub unsafe extern "C" fn plugin_destroy(handle: $crate::PluginHandle) {
            if handle.is_null() { return; }
            // cast back to Box<$ty> and drop it
            let raw: *mut $ty = handle as *mut $ty;
            drop(Box::from_raw(raw));
        }

        /// Plugin authors implement this in their `#[typetag]` impl:
        #[no_mangle]
        pub unsafe extern "C" fn plugin_set_logger(handle: $crate::PluginHandle, logger: PluginLogger) {
            if handle.is_null() {
                return;
            }
            // plugin is really a pointer to your concrete type
            let plugin = unsafe{&mut *(handle as *mut $ty) as &mut dyn ChannelPlugin};
            plugin.set_logger(logger);
        }

        #[no_mangle]
        pub unsafe extern "C" fn plugin_name(
            handle: $crate::PluginHandle
        ) -> *mut c_char {
            if handle.is_null() {
                return std::ptr::null_mut();
            }
            let plugin = &*(handle as *const $ty);
            let name: String = < $ty as $crate::plugin::ChannelPlugin >::name(plugin);

            CString::new(name).unwrap().into_raw()
        }

        // 3) The `send` wrapper:
        #[no_mangle]
        pub unsafe extern "C" fn plugin_send(
            handle: $crate::PluginHandle,
            msg: *const $crate::message::ChannelMessage,
        ) -> bool {
            if handle.is_null() || msg.is_null() { return false; }
            let plugin = &mut *(handle as *mut $ty);
            let message = &*msg;
            // fully‐qualified call to the trait impl
            < $ty as $crate::plugin::ChannelPlugin >::send(plugin, message.clone())
                .is_ok()
        }

        // 4) The `poll` wrapper:
        #[no_mangle]
        pub unsafe extern "C" fn plugin_poll(
            handle: $crate::PluginHandle,
            out_msg: *mut $crate::message::ChannelMessage,
        ) -> bool {
            if handle.is_null() || out_msg.is_null() { return false; }
            let plugin = &mut *(handle as *mut $ty);
            match < $ty as $crate::plugin::ChannelPlugin >::poll(plugin) {
                Ok(m) => {
                    ptr::write(out_msg, m);
                    true
                }
                Err(_) => false
            }
        }

        // 5) The `capabilities` wrapper:
        #[no_mangle]
        pub unsafe extern "C" fn plugin_capabilities(
            handle: $crate::PluginHandle,
            out: *mut $crate::message::ChannelCapabilities,
        ) -> bool {
            if handle.is_null() || out.is_null() { return false; }
            let plugin = &*(handle as *const $ty);
            let caps = < $ty as $crate::plugin::ChannelPlugin >::capabilities(plugin);
            ptr::write(out, caps);
            true
        }

        // 6) `config` and `secrets` → JSON strings:
        #[no_mangle]
        pub unsafe extern "C" fn plugin_set_config(
            handle: $crate::PluginHandle,
            json: *const c_char,
        ) {
            if handle.is_null() || json.is_null() { return; }
            let plugin = &mut *(handle as *mut $ty);
            let s = CStr::from_ptr(json).to_string_lossy();
            if let Ok(cfg) = serde_json::from_str(&s)
            {
                < $ty as $crate::plugin::ChannelPlugin>::set_config(plugin, cfg);
            }
        }

        /// NEW FFI: pass a JSON blob of the secrets map into the plugin
        #[no_mangle]
        pub unsafe extern "C" fn plugin_set_secrets(
            handle: $crate::PluginHandle,
            json: *const c_char,
        ) {
            if handle.is_null() || json.is_null() { return; }
            let plugin = &mut *(handle as *mut $ty);
            let s = CStr::from_ptr(json).to_string_lossy();
            if let Ok(sec) = serde_json::from_str(&s)
            {
                < $ty as $crate::plugin::ChannelPlugin>::set_secrets(plugin, sec);
            }
        }
        #[no_mangle]
        pub unsafe extern "C" fn plugin_free_string(s: *mut c_char) {
            if !s.is_null() {
                drop(CString::from_raw(s));
            }
        }

        // 6.1) List the config keys the plugin wants:
        #[no_mangle]
        pub unsafe extern "C" fn plugin_list_config(
            handle: $crate::PluginHandle
        ) -> *mut c_char {
            if handle.is_null() {
                return std::ptr::null_mut();
            }
            let plugin = &*(handle as *const $ty);
            let list: Vec<String> = < $ty as $crate::plugin::ChannelPlugin >::list_config(plugin);
            match serde_json::to_string(&list) {
                Ok(js) => {
                    // allocate a C string and return its pointer
                    CString::new(js).unwrap().into_raw()
                }
                Err(_) => std::ptr::null_mut(),
            }
        }

        // 6.2) List the secret keys the plugin wants:
        #[no_mangle]
        pub unsafe extern "C" fn plugin_list_secrets(
            handle: $crate::PluginHandle
        ) -> *mut c_char {
            if handle.is_null() {
                return std::ptr::null_mut();
            }
            let plugin = &*(handle as *const $ty);
            let list: Vec<String> = < $ty as $crate::plugin::ChannelPlugin >::list_secrets(plugin);
            match serde_json::to_string(&list) {
                Ok(js) => CString::new(js).unwrap().into_raw(),
                Err(_) => std::ptr::null_mut(),
            }
        }


        // 8) `state`, `start`, `drain`, `wait_until_drained`, `stop`:
        #[no_mangle]
        pub unsafe extern "C" fn plugin_state(
            handle: $crate::PluginHandle,
        ) -> $crate::plugin::ChannelState {
            if handle.is_null() {
                return $crate::plugin::ChannelState::Stopped;
            }
            let plugin = &*(handle as *const $ty);
            < $ty as $crate::plugin::ChannelPlugin >::state(plugin)
        }

       #[no_mangle]
        pub unsafe extern "C" fn plugin_start(handle: $crate::PluginHandle) -> bool {
            if handle.is_null() { return false; }
            let plugin = &mut *(handle as *mut $ty);
            < $ty as ChannelPlugin>::start(plugin).is_ok()
        }

        #[no_mangle]
        pub unsafe extern "C" fn plugin_drain(handle: $crate::PluginHandle) -> bool {
            if handle.is_null() { return false; }
            let plugin = &mut *(handle as *mut $ty);
            < $ty as ChannelPlugin>::drain(plugin).is_ok()
        }

        #[no_mangle]
        pub unsafe extern "C" fn plugin_wait_until_drained(
            handle: $crate::PluginHandle,
            timeout_ms: u64,
        ) -> bool {
            if handle.is_null() { return false; }
            let plugin = &mut *(handle as *mut $ty);
            < $ty as ChannelPlugin>::wait_until_drained(plugin, timeout_ms).is_ok()
        }

        #[no_mangle]
        pub unsafe extern "C" fn plugin_stop(handle: $crate::PluginHandle) -> bool {
            if handle.is_null() { return false; }
            let plugin = &mut *(handle as *mut $ty);
            < $ty as ChannelPlugin>::stop(plugin).is_ok()
        }


    };
}