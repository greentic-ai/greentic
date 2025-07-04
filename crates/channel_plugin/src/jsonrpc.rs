use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::error;

use crate::plugin_actor::Method;


/// JSON‑RPC 2.0 core types for Greentic plugins communicated over stdin/stdout.
///
/// These structs intentionally mirror the [JSON‑RPC 2.0 spec](https://www.jsonrpc.org/specification).
/// They are **transport‑agnostic** and can be reused for HTTP, WebSocket, or gRPC (via `prost` → JSON) if needed.
///
/// Usage example (with `serde_json`):
/// ```ignore
/// use serde_json::json;
/// use greentic_plugin::jsonrpc::{Id, Request};
///
/// let req = Request::call(Id::Number(1), "messageIn", Some(json!({"text": "hi"})));
/// let s = serde_json::to_string(&req).unwrap();
/// ```
pub const JSONRPC_VERSION: &str = "2.0";

/// `id` MAY be a string, number or null. We support all forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    String(String),
    Null,
}

/// JSON‑RPC 2.0 Request object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default = "default_version")]
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Omitted for *notifications*.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
}

fn default_version() -> String {
    JSONRPC_VERSION.to_owned()
}

/// JSON‑RPC 2.0 Error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON‑RPC 2.0 Response object.
/// Exactly one of `result` or `error` **must** be present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(default = "default_version")]
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
    pub id: Id,
}

/// Convenience enum so callers can `serde_json::from_str::<Message>()` without inspecting the type first.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Response(Response),
}

// -----------------------------------------------------------------------------
// Helper constructors – make it ergonomic to build requests and responses.
// -----------------------------------------------------------------------------
impl Request {
    /// Create a *notification* (no response expected).
    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
            id: None,
        }
    }

    /// Create a *call* expecting a response.
    pub fn call<M: Into<Method>>(id: Id, method: M, params: Option<Value>) -> Self {
        let method = method.into();
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.to_string(),
            params,
            id: Some(id),
        }
    }
}

impl Response {
    /// Convenience helper for a successful result.
    pub fn success(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Convenience helper for an error result.
    pub fn fail(id: Id, code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            result: None,
            error: Some(Error {
                code,
                message: message.into(),
                data,
            }),
            id,
        }
    }
}


/// Strongly‑typed list of JSON‑RPC method names used between the Plugin Manager
/// and plugin subprocesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginMethod {
    // Messaging
    MessageIn,
    MessageOut,

    // Lifecycle
    Init,
    Start,
    Drain,
    Stop,
    Status,
    Health,
    WaitUntilDrained,
    Capabilities,

    // Config / Secrets
    SetConfig,
    SetSecrets,
    ListConfigKeys,
    ListSecretKeys,

    // Session / Routing
    GetSession,
    InvalidateSession,
}

impl PluginMethod {
    /// Returns the canonical JSON‑RPC method string.
    pub const fn as_str(&self) -> &'static str {
        match self {
            PluginMethod::MessageIn => "messageIn",
            PluginMethod::MessageOut => "messageOut",
            PluginMethod::Init => "init",
            PluginMethod::Start => "start",
            PluginMethod::Drain => "drain",
            PluginMethod::Stop => "stop",
            PluginMethod::Status => "status",
            PluginMethod::Health => "health",
            PluginMethod::Capabilities => "capabilities",
            PluginMethod::WaitUntilDrained => "waitUntilDrained",
            PluginMethod::SetConfig => "setConfig",
            PluginMethod::SetSecrets => "setSecrets",
            PluginMethod::ListConfigKeys => "listConfigKeys",
            PluginMethod::ListSecretKeys => "listSecretKeys",
            PluginMethod::GetSession => "getSession",
            PluginMethod::InvalidateSession => "invalidateSession",
        }
    }
}

impl From<PluginMethod> for String {
    fn from(m: PluginMethod) -> Self {
        m.as_str().to_owned()
    }
}

impl std::str::FromStr for PluginMethod {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "messageIn" => Ok(PluginMethod::MessageIn),
            "messageOut" => Ok(PluginMethod::MessageOut),
            "init" => Ok(PluginMethod::Init),
            "start" => Ok(PluginMethod::Start),
            "drain" => Ok(PluginMethod::Drain),
            "stop" => Ok(PluginMethod::Stop),
            "status" => Ok(PluginMethod::Status),
            "health" => Ok(PluginMethod::Health),
            "capabilities" => Ok(PluginMethod::Capabilities),
            "waitUntilDrained" => Ok(PluginMethod::WaitUntilDrained),
            "setConfig" => Ok(PluginMethod::SetConfig),
            "setSecrets" => Ok(PluginMethod::SetSecrets),
            "listConfigKeys" => Ok(PluginMethod::ListConfigKeys),
            "listSecretKeys" => Ok(PluginMethod::ListSecretKeys),
            "getSession" => Ok(PluginMethod::GetSession),
            "invalidateSession" => Ok(PluginMethod::InvalidateSession),
            other => {
                error!("Fix bug in PluginMethod FromStr for {}",other);
                Err(())
            },
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_request() {
        let req = Request::call(Id::Number(1), Method::Name, Some(json!({"msg": "hi"})));
        let s = serde_json::to_string(&req).unwrap();
        let de: Request = serde_json::from_str(&s).unwrap();
        assert_eq!(de.method, "name");
    }

    #[test]
    fn roundtrip_response() {
        let resp = Response::success(Id::String("abc".into()), json!({"ok": true}));
        let s = serde_json::to_string(&resp).unwrap();
        let de: Response = serde_json::from_str(&s).unwrap();
        assert_eq!(de.result.unwrap()["ok"], json!(true));
    }

    #[test]
    fn plugin_method_parse() {
        let m: PluginMethod = "health".parse().unwrap();
        assert_eq!(m, PluginMethod::Health);
        assert_eq!(m.as_str(), "health");
    }
}
