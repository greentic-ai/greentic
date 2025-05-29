
// plugin_api/src/lib.rs
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, ffi::{c_char, c_void, CString}};
use thiserror::Error;
use crate::{message::{ChannelCapabilities, ChannelMessage}, PluginHandle};


#[derive(Debug, Copy, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[repr(C)]
pub enum ChannelState { 
    Starting, 
    Running, 
    Draining, 
    #[default]
    Stopped }

/// What log levels are supported?  
/// Higher‐value variants are more severe.
#[repr(C)]
#[derive(Debug, Copy, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Critical = 5,
}


/// Plain‐old‐data FFI logger handle.
/// Plugins will call `log_fn(ctx, level, context, message)` when they want to log.
#[repr(C)]
pub struct PluginLogger {
    /// opaque pointer you get back in your callback
    pub ctx: *mut c_void,
    /// callback function pointer
    pub log_fn: extern "C" fn(ctx: *mut c_void,
                            level: LogLevel,
                            context: *const c_char,
                            message: *const c_char),
}

impl PluginLogger {
    /// Safely log without writing any unsafe in the plugin.
    pub fn log(&self, level: LogLevel, context: &str, message: &str) {
        // NUL‐terminate our Rust strings for the C boundary
        let c_ctx     = CString::new(context).expect("context contained a NUL");
        let c_message = CString::new(message).expect("message contained a NUL");

        // Call the FFI function pointer
        (self.log_fn)(
            self.ctx,
            level,
            c_ctx.as_ptr(),
            c_message.as_ptr(),
        );
    
        
    }
}

unsafe impl Send for PluginLogger {}
unsafe impl Sync for PluginLogger {}

/// New FFI entry‐point you must expose:
pub type PluginSetLogger = unsafe extern "C" fn(PluginHandle, PluginLogger);

/// The one trait plugin authors implement.
pub trait ChannelPlugin: Send + Sync {
    /// The name of the plugin
    fn name(&self) -> String;

    /// Called by the host to give the plugin a logger handle.
    /// Plugins should store this in their struct and use it in place of
    /// any direct calls to `tracing::info!`.
    fn set_logger(&mut self, logger: PluginLogger);

    /// Called by host to push a message out.
    /// If the to is left blank but the msg.session_id is set
    /// the message should be send back to the user who started
    /// the original converation (poll(session_id)).
    fn send(&mut self, msg: ChannelMessage) -> anyhow::Result<(),PluginError>;

    /// Block until next message (or Err if stopped).
    /// The session_id can be used to link internal identifier like
    /// a chat_id in Telegrm to a session
    fn poll(&self) -> anyhow::Result<ChannelMessage,PluginError>;

    /// Metadata about this channel.
    fn capabilities(&self) -> ChannelCapabilities;

    /// Receive the full configuration map.
    fn set_config(&mut self, config: HashMap<String, String>);

    /// Lists the configs required
    fn list_config(&self) -> Vec<String>;

    /// Receive the full secrets map.
    fn set_secrets(&mut self, secrets: HashMap<String, String>);

    /// Lists the secrets required
    fn list_secrets(&self) -> Vec<String>;

    /// What state are we in?
    fn state(&self) -> ChannelState;

    /// Start up underlying connections.
    fn start(&mut self) -> Result<(),PluginError>;

    /// Stop taking new messages.
    fn drain(&mut self) -> Result<(),PluginError>;

    /// Block until all in-flight messages are done or PluginError if timeout is reached.
    fn wait_until_drained(&mut self, timeout_ms: u64) -> Result<(),PluginError>;

    /// Kill immediately.
    fn stop(&mut self) -> Result<(),PluginError>;
}

/// Errors that a ChannelPlugin implementation can return.
#[derive(Error, Debug, Serialize, Deserialize, JsonSchema)]
#[repr(C)]
pub enum PluginError {
    /// Something went wrong sending or receiving JSON.
    #[error("JSON error: {0}")]
    Json(String),

    /// The plugin is not in a state where this operation is valid.
    #[error("invalid state for this operation")]
    InvalidState,

    /// A timeout occurred.
    #[error("operation timed out after {0} ms")]
    Timeout(u64),

    /// The plugin returned an unspecified failure.
    #[error("plugin error: {0}")]
    Other(String),
}

impl From<serde_json::Error> for PluginError {
    fn from(err: serde_json::Error) -> PluginError {
        PluginError::Json(err.to_string())
    }
}

impl From<anyhow::Error> for PluginError {
    fn from(err: anyhow::Error) -> PluginError {
        PluginError::Other(err.to_string())
    }
}