
use async_trait::async_trait;
use dashmap::DashMap;
// plugin_api/src/lib.rs
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, c_void, CString};
use thiserror::Error;
use crate::{message::{ChannelCapabilities, ChannelMessage, RouteBinding, RouteMatcher}, PluginHandle};
use std::sync::RwLock;


#[derive(Debug, Serialize, Deserialize, JsonSchema, Default)]
pub struct DefaultRoutingSupport {
    routes: RwLock<Vec<RouteBinding>>,
}

impl RoutingSupport for DefaultRoutingSupport {
    fn add(&self, route: RouteBinding) {
        let mut routes = self.routes.write().unwrap();
        routes.push(route);
    }

    fn remove(&self, flow: &str, node: &str) {
        let mut routes = self.routes.write().unwrap();
        routes.retain(|r| r.flow != flow || r.node != node);
    }

    fn list(&self) -> Vec<RouteBinding> {
        self.routes.read().unwrap().clone()
    }

    fn set(&self, new_routes: Vec<RouteBinding>) {
        let mut routes = self.routes.write().unwrap();
        *routes = new_routes;
    }

    fn find_match(&self, matcher: &RouteMatcher) -> Option<RouteBinding> {
        let routes = self.routes.read().unwrap();

        for route in routes.iter() {
            match (&route.matcher, matcher) {
                (RouteMatcher::Command(expected), RouteMatcher::Command(actual)) if expected == actual => {
                    return Some(route.clone());
                }
                (RouteMatcher::ThreadId(expected), RouteMatcher::ThreadId(actual)) if expected == actual => {
                    return Some(route.clone());
                }
                (RouteMatcher::Participant(expected), RouteMatcher::Participant(actual)) if expected == actual => {
                    return Some(route.clone());
                }
                (RouteMatcher::WebPath(expected), RouteMatcher::WebPath(actual)) if expected == actual => {
                    return Some(route.clone());
                }
                (RouteMatcher::Custom(expected), RouteMatcher::Custom(actual)) if expected == actual => {
                    return Some(route.clone());
                }
                _ => continue,
            }
        }

        None
    }
}

impl Clone for DefaultRoutingSupport {
    fn clone(&self) -> Self {
        let routes = self.routes.read().unwrap().clone();
        Self {
            routes: RwLock::new(routes),
        }
    }
}

impl PartialEq for DefaultRoutingSupport {
    fn eq(&self, other: &Self) -> bool {
        let a = self.routes.read().unwrap();
        let b = other.routes.read().unwrap();
        *a == *b
    }
}

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
#[derive(Clone, Copy, Debug)]
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

pub trait RoutingSupport {
    fn add(&self, route: RouteBinding);
    fn remove(&self, flow: &str, node: &str);
    fn list(&self) -> Vec<RouteBinding>;
    fn set(&self, new_routes: Vec<RouteBinding>);
    fn find_match(&self, matcher: &RouteMatcher) -> Option<RouteBinding>;
}


/// The one trait plugin authors implement.
#[async_trait]
pub trait ChannelPlugin {//: Send + Sync {
    /// The name of the plugin
    fn name(&self) -> String;

    /// Whether this plugin supports dynamic routing updates or not.
    /// needs to be set in the ChannelCapabilities
    fn supports_routing(&self) -> bool {
        self.capabilities().supports_routing
    }

    /// Does the plugin support routing. Overwrite this fn with your
    /// routing support implementation if you don't want to use the default. 
    /// Otherwise you can simply do:
    /// pub struct YourPlugin {
    ///     routing: DefaultRoutingSupport,
    ///     ...
    /// }
    /// add to your default or new fn 
    ///   routing: DefaultRoutingSupport::default(),
    /// 
    /// #[async_trait]
    /// impl ChannelPlugin for YourPlugin {
    ///   fn get_routing_support(&self) -> Option<&dyn RoutingSupport> {
    ///      Some(&self.routing)
    ///   }
    ///   ...
    /// }
    /// Afterwards when a receive_message is called you need to use
    /// the matching to set the node and flow to route to. See also
    /// MessagingRouteContext for an easy way to define different routing
    /// options.
    fn get_routing_support(&self) -> Option<&dyn RoutingSupport> {
        None
    }

    fn set_routes(&mut self, routes: Vec<RouteBinding>) -> Result<(), PluginError> {
        if let Some(routing) = self.get_routing_support() {
            routing.set(routes);
            Ok(())
        } else {
            Err(PluginError::NotSupported)
        }
    }

    fn remove_route(&self, flow: &str, node: &str) {
        if let Some(routing) = self.get_routing_support() {
            routing.remove(flow, node);
        }
    }

    fn add_route(&self, route: RouteBinding) {
        if let Some(routing) = self.get_routing_support() {
            routing.add(route);
        }
    }

    fn list_routes(&self) -> Vec<RouteBinding> {
        if let Some(routing) = self.get_routing_support() {
            routing.list()
        } else {
            Vec::new()
        }
    }

    /// Called by the host to give the plugin a logger handle.
    /// Plugins should store this in their struct and use it in place of
    /// any direct calls to `tracing::info!`.
    fn set_logger(&mut self, logger: PluginLogger);
    fn get_logger(&self) -> Option<PluginLogger>;

    /// Called by host to push a message out.
    async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(),PluginError>;

    /// Called by the host to receive an incoming message
    async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage,PluginError>;

    /// Metadata about this channel.
    fn capabilities(&self) -> ChannelCapabilities;

    /// Receive the full configuration map.
    fn set_config(&mut self, config: DashMap<String, String>);

    /// Lists the configs required
    fn list_config(&self) -> Vec<String>;

    /// Receive the full secrets map.
    fn set_secrets(&mut self, secrets: DashMap<String, String>);

    /// Lists the secrets required
    fn list_secrets(&self) -> Vec<String>;

    /// What state are we in?
    fn state(&self) -> ChannelState;

    /// Start up underlying connections.
    async fn start(&mut self) -> Result<(),PluginError>;

    /// Stop taking new messages.
    fn drain(&mut self) -> Result<(),PluginError>;

    /// Block until all in-flight messages are done or PluginError if timeout is reached.
    async fn wait_until_drained(&mut self, timeout_ms: u64) -> Result<(),PluginError>;

    /// Kill immediately.
    async fn stop(&mut self) -> Result<(),PluginError>;
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

    /// The plugin does not support a certain feature.
    #[error("this feature is not supported by this plugin")]
    NotSupported,
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