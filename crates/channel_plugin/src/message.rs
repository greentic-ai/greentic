//! Domain‑specific types for Greentic ↔ Plugin JSON‑RPC messaging.
//!
//! These structs complement the core JSON‑RPC types in `jsonrpc.rs` and are used
//! inside the `params` / `result` fields of JSON‑RPC requests and responses.
//!
//! All structs derive **serde** `Serialize` / `Deserialize` so they can be embedded
//! directly as `serde_json::Value` when constructing JSON‑RPC envelopes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// -----------------------------------------------------------------------------
// Core (re‑used) helper types
// -----------------------------------------------------------------------------

/// Trace context propagated end‑to‑end for observability.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct TraceInfo {
    pub trace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

// -----------------------------------------------------------------------------
// Participants & File / Media metadata
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema,PartialEq)]
pub struct Participant {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_specific_id: Option<String>,
}

impl Participant {
    pub fn new(id: String, display_name: Option<String>, channel_specific_id: Option<String>) -> Self {
        Self{id, display_name,channel_specific_id}
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema,)]
pub struct FileMetadata {
    pub file_name: String,
    pub mime_type: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema,)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaType {
    IMAGE,
    VIDEO,
    AUDIO,
    BINARY,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema,)]
pub struct MediaMetadata {
    pub kind: MediaType,
    pub file: FileMetadata,
}

// -----------------------------------------------------------------------------
// Message content variants
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema,)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageContent {
    Text { text: String },
    File { file: FileMetadata },
    Media { media: MediaMetadata },
    Event { event: Event },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema,)]
pub struct Event {
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub event_payload: Value,
}

// -----------------------------------------------------------------------------
// ChannelMessage – the core envelope exchanged with external channels
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema,PartialEq)]
pub struct ChannelMessage {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub direction: MessageDirection,
    pub channel: String,
    pub from: Participant,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<Participant>,
    /// RFC‑3339/ISO‑8601 UTC timestamp (e.g. "2025‑06‑25T12:34:56Z").
    pub timestamp: String,
    pub content: Vec<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
}

// -----------------------------------------------------------------------------
// ChannelMessage – the core envelope exchanged with external channels
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct EventType {
    pub event_type: String,             // e.g., "UserJoined", "TypingStarted"
    pub description: String,            // Human-readable description
    pub payload_schema: Option<Value>,  // the json schema for the event_payload  
}
#[derive(Debug, Clone, Serialize, Deserialize,  Default, JsonSchema,)]
pub struct ChannelCapabilities {
    pub name: String,                         // e.g. "Slack", "Email", "SMS"
    pub supports_sending: bool,
    pub supports_receiving: bool,
    pub supports_text: bool,
    pub supports_files: bool,
    pub supports_media: bool,
    pub supports_events: bool,
    pub supports_typing: bool,
    pub supports_routing: bool,
    pub supports_threading: bool,
    pub supports_reactions: bool,
    pub supports_call: bool,
    pub supports_buttons: bool,
    pub supports_links: bool,
    pub supports_custom_payloads: bool,

    pub supported_events: Vec<EventType>, // List of events with descriptions
}

// -----------------------------------------------------------------------------
// JSON‑RPC method payloads
// -----------------------------------------------------------------------------


/// Result for `messageIn`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct MessageInResult {
    pub message: ChannelMessage,
    pub error: bool,
}

/// Params for `messageOut` (Manager → Plugin)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct MessageOutParams {
    pub message: ChannelMessage,

}

/// Result for `messageOut`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct MessageOutResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}


#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct TextMessage {
    pub text: String
}


// -----------------------------------------------------------------------------
// JSON‑RPC method payloads – Lifecycle
// -----------------------------------------------------------------------------

/// High‑level status values for plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChannelState {
    STARTING,
    RUNNING,
    DRAINING,
    #[default]
    STOPPED,
    FAILED,
}

/// Syslog-style log levels in ascending order of severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageDirection {
    #[default]
    Incoming,
    Outgoing,
}

/// Params for `init` (Manager → Plugin)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct InitParams {
    /// Plugin version string (semver).
    pub version: String,
    /// Configuration key‑values provided at start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<(String, String)>,
    /// Secrets provided at start (already decrypted / fetched).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<(String, String)>,
    /// Minimum level that will be recorded.
    pub log_level: LogLevel,
    /// Optional directory to write rotating log files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<String>,
    /// Optional future extension for opentelemetry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otel_endpoint: Option<String>,
}

/// Result in an error when a init fails, e.g. logging, config, secrets
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct InitResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Result is an error when a drain fails
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct DrainResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Result is an error when a stop fails
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct StopResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Result for `version` (Plugin → Manager)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct VersionResult {
    pub version: String,
}


/// Result for `status` (Plugin → Manager)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct StateResult {
    pub state: ChannelState,
}

/// Result for `health` (Plugin → Manager)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct HealthResult {
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// -----------------------------------------------------------------------------
// JSON‑RPC method payloads – Drain
// -----------------------------------------------------------------------------

/// Params for `waitUntilDrained`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct WaitUntilDrainedParams {
    /// milliseconds to wait until drained
    pub timeout_ms: u64,
}

/// Result is stopped true and error false if stopped correctly and stopped false and error true if timeout
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct WaitUntilDrainedResult {
    pub stopped: bool,
    pub error: bool,
}

// -----------------------------------------------------------------------------
// JSON‑RPC method payloads – Config / Secrets / Session
// -----------------------------------------------------------------------------

/// Params for `setConfig`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct SetConfigParams {
    /// Key‑value map of configuration values.
    pub config: Vec<(String, String)>,
}

/// Params for `setSecrets`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct SetSecretsParams {
    pub secrets: Vec<(String, String)>,
}

/// Returns the required and optional keys should be set
/// For each key, there is also an optional description of what the key is for
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema,)]
pub struct ListKeysResult {
    pub required_keys: Vec<(String,Option<String>)>, // key and optional description
    pub optional_keys: Vec<(String,Option<String>)>, // key and optional description
}


/// Result in an error when a required config item is not set
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct SetConfigResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Returns the config keys that can be set
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct ListConfigKeysResult {
    pub keys: Vec<String>,
}

/// Result in an error when a required secret is not set
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct SetSecretsResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Result the plugin name
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct NameResult {
    pub name: String,
}

/// Result the capabilities
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct CapabilitiesResult {
    pub capabilities: ChannelCapabilities,
}

/// Allows for explicit invalidation of a greentic session
/// Normally sessions will be started on user join and invalidated on user left 
/// but plugins can be explicit about invalidating sessions
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema,)]
pub struct InvalidateSessionParams {
    /// Full external session key: plugin|external_key
    pub key: String,
}
/// Combined plugin/key session identifier (format: `plugin_id|external_key`).
/// Example: `telegram|user_12345`
pub fn make_session_key(plugin_id: &str, key: &str) -> String {
    format!("{plugin_id}|{key}")
}

// -----------------------------------------------------------------------------
// Tests – basic round‑trips to ensure serde compatibility
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn channel_message_roundtrip() {
        let cm = ChannelMessage {
            id: "1".into(),
            session_id: None,
            direction: MessageDirection::Incoming,
            channel: "telegram".into(),
            from: Participant { id: "user".into(), display_name: None, channel_specific_id: None },
            to: vec![],
            timestamp: "2025-06-25T12:00:00Z".into(),
            content: vec![MessageContent::Text { text: "hi".into() }],
            thread_id: None,
            reply_to_id: None,
            metadata: json!({}),
        };
        let js = serde_json::to_string(&cm).unwrap();
        let de: ChannelMessage = serde_json::from_str(&js).unwrap();
        assert_eq!(de.id, "1");
    }
}

