// src/main.rs

use std::ffi::c_void;
use dashmap::DashMap;
use channel_plugin::{fakse_session::{fake_get_or_create_session, fake_get_session, fake_invalidate_session}, plugin::{run_blocking, ChannelPlugin, LogLevel, PluginLogger}};
use channel_ws::{greentic_register_session_fns, WsPlugin};

extern "C" fn test_log_fn(
    _ctx: *mut c_void,
    level: LogLevel,
    tag: *const i8,
    msg: *const i8,
) {
    use std::ffi::CStr;
    let tag = unsafe { CStr::from_ptr(tag).to_string_lossy() };
    let msg = unsafe { CStr::from_ptr(msg).to_string_lossy() };
    println!("[{:?}] {}: {}", level, tag, msg);
}


fn main() {
    // 1) Create and configure the plugin
    let mut plugin = WsPlugin::default();
    greentic_register_session_fns(
        fake_get_session,
        fake_get_or_create_session,
        fake_invalidate_session,
    );
    let cfg = DashMap::new();
    cfg.insert("address".into(), "0.0.0.0".into());
    cfg.insert("port".into(), "8888".into());
    plugin.set_config(cfg);

    // 2) Install a simple stdout logger
    let ffi_logger = PluginLogger {
        ctx: std::ptr::null_mut(),
        log_fn: test_log_fn,
    };
    plugin.set_logger(ffi_logger, LogLevel::Debug);
    let _ = run_blocking(async move {
        // 3) Start the WebSocket server
        println!("Starting WS server on {}", plugin.address());
        plugin.start().await.map_err(|e| anyhow::anyhow!("start failed: {:?}", e)).expect("can not start");
        println!("WS server is running; press Ctrl-C to shut down");

        // 4) Wait for Ctrl-C
        tokio::signal::ctrl_c().await.expect("can not listen for ctrl-c");
        println!("\nShutting down WS server…");

        // 5) Tear down
        plugin.stop().await.expect("cannot stop");
        println!("Goodbye!");

     });
}
