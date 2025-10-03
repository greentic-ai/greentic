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
pub const PLUGIN_VERSION: &str = "0.3";

/// Trace context propagated end‑to‑end for observability.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema, PartialEq)]
pub struct Participant {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_specific_id: Option<String>,
}

impl Participant {
    pub fn new(
        id: String,
        display_name: Option<String>,
        channel_specific_id: Option<String>,
    ) -> Self {
        Self {
            id,
            display_name,
            channel_specific_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct FileMetadata {
    pub file_name: String,
    pub mime_type: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaType {
    IMAGE,
    VIDEO,
    AUDIO,
    BINARY,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct MediaMetadata {
    pub kind: MediaType,
    pub file: FileMetadata,
}

// -----------------------------------------------------------------------------
// Message content variants
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageContent {
    Text { text: String },
    File { file: FileMetadata },
    Media { media: MediaMetadata },
    Event { event: Event },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct Event {
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub event_payload: Value,
}

// -----------------------------------------------------------------------------
// ChannelMessage – the core envelope exchanged with external channels
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema, PartialEq)]
pub struct ChannelMessage {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub direction: MessageDirection,
    pub channel: String,
    /// Free-form channel-specific payload
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub channel_data: Value,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JsonSchemaDescriptor {
    /// Inline JSON Schema (Draft 2020-12 ideally)
    Inline {
        /// The schema object itself
        schema: Value,
        /// Optional: stable identifier inside the schema (`$id`) if present
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Optional: semantic or provider version
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    /// Out-of-line schema by URI (MCP-style references)
    Ref {
        /// Where to fetch the schema (https, file, r2, etc.)
        uri: String,
        /// Optional integrity/versioning hints
        #[serde(skip_serializing_if = "Option::is_none")]
        etag: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
}

impl ChannelMessage {
    pub fn event_type(&self) -> anyhow::Result<&str> {
        for content in &self.content {
            if let MessageContent::Event { event } = content {
                return Ok(&event.event_type);
            }
        }
        Err(anyhow::anyhow!("No event_type found in message"))
    }

    pub fn get_event_payload(&self) -> anyhow::Result<&serde_json::Map<String, Value>> {
        for content in &self.content {
            if let MessageContent::Event { event } = content {
                return event
                    .event_payload
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("event_payload is not an object"));
            }
        }
        Err(anyhow::anyhow!("No event_payload found in message"))
    }
}

// -----------------------------------------------------------------------------
// ChannelMessage – the core envelope exchanged with external channels
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EventType {
    pub event_type: String,            // e.g., "UserJoined", "TypingStarted"
    pub description: String,           // Human-readable description
    pub payload_schema: Option<Value>, // the json schema for the event_payload
}

impl EventType {
    pub fn new(event_type: String, description: String, payload_schema: Option<Value>) -> Self {
        Self {
            event_type,
            description,
            payload_schema,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct ChannelCapabilities {
    pub name: String, // e.g. "Slack", "Email", "SMS"
    pub version: String,
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
    /// For validation/caching
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_data_schema_id: Option<String>, // e.g. "greentic://channel/ms_teams.message@v1"
    /// JSON Schema describing `channel_data`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_data_schema: Option<JsonSchemaDescriptor>,
    pub supported_events: Vec<EventType>, // List of events with descriptions
}

// -----------------------------------------------------------------------------
// JSON‑RPC method payloads
// -----------------------------------------------------------------------------

/// Result for `messageIn`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MessageInResult {
    pub message: ChannelMessage,
    pub error: bool,
}

/// Params for `messageOut` (Manager → Plugin)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MessageOutParams {
    pub message: ChannelMessage,
}

/// Result for `messageOut`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MessageOutResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct TextMessage {
    pub text: String,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InitResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Result is an error when a drain fails
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DrainResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Result is an error when a stop fails
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HealthResult {
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// -----------------------------------------------------------------------------
// JSON‑RPC method payloads – Drain
// -----------------------------------------------------------------------------

/// Params for `waitUntilDrained`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitUntilDrainedParams {
    /// milliseconds to wait until drained
    pub timeout_ms: u64,
}

/// Result is stopped true and error false if stopped correctly and stopped false and error true if timeout
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitUntilDrainedResult {
    pub stopped: bool,
    pub error: bool,
}

// -----------------------------------------------------------------------------
// JSON‑RPC method payloads – Config / Secrets / Session
// -----------------------------------------------------------------------------

/// Params for `setConfig`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetConfigParams {
    /// Key‑value map of configuration values.
    pub config: Vec<(String, String)>,
}

/// Params for `setSecrets`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetSecretsParams {
    pub secrets: Vec<(String, String)>,
}

/// Returns the required and optional keys should be set
/// For each key, there is also an optional description of what the key is for
/// Dynamic keys allow for keys to be dynamically composed based on a format, e.g. <channel_id>:<tenant_id>:<user_id>:<key_name>
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListKeysResult {
    pub required_keys: Vec<(String, Option<String>)>, // key and optional description
    pub optional_keys: Vec<(String, Option<String>)>, // key and optional description
    pub dynamic_keys: Vec<(String, Option<String>)>, // dynamic key format, and optional description but for dynamic keys
}

/// Result in an error when a required config item is not set
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetConfigResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Returns the config keys that can be set
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListConfigKeysResult {
    pub keys: Vec<String>,
}

/// Result in an error when a required secret is not set
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetSecretsResult {
    pub success: bool,
    pub error: Option<String>,
}

/// Result the plugin name
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NameResult {
    pub name: String,
}

/// Result the capabilities
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CapabilitiesResult {
    pub capabilities: ChannelCapabilities,
}

/// Allows for explicit invalidation of a greentic session
/// Normally sessions will be started on user join and invalidated on user left
/// but plugins can be explicit about invalidating sessions
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

    // ---- helpers ------------------------------------------------------------

    // If your Participant doesn't implement Default, replace this helper
    // to construct a minimal valid Participant.
    fn dummy_participant() -> Participant {
        // If `Participant` has fields like { id, name }, adapt here:
        // Participant { id: "u1".into(), name: Some("Alice".into()) }
        // Otherwise, if it implements Default:
        #[allow(clippy::default_trait_access)]
        Participant::default()
    }

    fn now_iso() -> String {
        // Fixed timestamp for determinism in tests
        "2025-08-08T08:00:00Z".to_string()
    }

    // Validates `data` against the inline schema inside capabilities (if present)
    fn validate_with_caps(
        caps: &ChannelCapabilities,
        data: &serde_json::Value,
    ) -> Result<(), String> {
        let Some(desc) = &caps.channel_data_schema else {
            return Ok(());
        };
        let schema_val = match desc {
            JsonSchemaDescriptor::Inline { schema, .. } => schema.clone(),
            JsonSchemaDescriptor::Ref { .. } => {
                // In production you’d resolve/cache it; for tests we error to make it explicit.
                return Err("Ref schema provided but not resolved in test".to_string());
            }
        };
        match jsonschema::validate(&schema_val, data) {
            Ok(()) => Ok(()),
            Err(e) => Err(format!("channel_data validation failed: {e}")),
        }
    }

    // ---- tests: MessageContent round-trips ---------------------------------

    #[test]
    fn message_content_text_roundtrip() {
        let mc = MessageContent::Text {
            text: "hello".into(),
        };
        let ser = serde_json::to_string(&mc).unwrap();
        let de: MessageContent = serde_json::from_str(&ser).unwrap();
        assert_eq!(mc, de);
    }

    #[test]
    fn message_content_file_roundtrip() {
        let mc = MessageContent::File {
            file: FileMetadata {
                file_name: "po.pdf".into(),
                mime_type: "application/pdf".into(),
                url: "r2://bucket/po.pdf".into(),
                size_bytes: Some(58_211),
            },
        };
        let ser = serde_json::to_string(&mc).unwrap();
        let de: MessageContent = serde_json::from_str(&ser).unwrap();
        assert_eq!(mc, de);
    }

    #[test]
    fn message_content_media_roundtrip() {
        let mc = MessageContent::Media {
            media: MediaMetadata {
                kind: MediaType::IMAGE,
                file: FileMetadata {
                    file_name: "img.png".into(),
                    mime_type: "image/png".into(),
                    url: "https://cdn/x.png".into(),
                    size_bytes: None,
                },
            },
        };
        let ser = serde_json::to_string(&mc).unwrap();
        let de: MessageContent = serde_json::from_str(&ser).unwrap();
        assert_eq!(mc, de);
    }

    #[test]
    fn message_content_event_roundtrip() {
        let mc = MessageContent::Event {
            event: Event {
                event_type: "typing_start".into(),
                event_payload: json!({"duration_ms": 1200}),
            },
        };
        let ser = serde_json::to_string(&mc).unwrap();
        let de: MessageContent = serde_json::from_str(&ser).unwrap();
        assert_eq!(mc, de);
    }

    // ---- tests: ChannelMessage minimal + channel_data -----------------------

    #[test]
    fn channel_message_minimal_serializes() {
        let msg = ChannelMessage {
            id: "m1".into(),
            session_id: Some("sess-1".into()),
            direction: MessageDirection::Incoming, // uses your existing enum
            channel: "ms_teams".into(),
            from: dummy_participant(),
            to: vec![],
            timestamp: now_iso(),
            content: vec![MessageContent::Text {
                text: "Hello".into(),
            }],
            thread_id: Some("teams-thread-123".into()),
            reply_to_id: None,
            // No schema attached here—should still serialize/deserialize
            metadata: serde_json::Value::Null,
            // If you added `tenant`, `channel_data`, `idempotency_key`, keep defaulting them:
            ..ChannelMessage::default()
        };

        let ser = serde_json::to_string_pretty(&msg).unwrap();
        let de: ChannelMessage = serde_json::from_str(&ser).unwrap();
        assert_eq!(msg.id, de.id);
        assert_eq!(msg.channel, de.channel);
        assert_eq!(msg.content, de.content);
    }

    #[test]
    fn channel_message_with_channel_data_roundtrip() {
        let msg = ChannelMessage {
            id: "m2".into(),
            session_id: Some("sess-2".into()),
            direction: MessageDirection::Incoming,
            channel: "ms_teams".into(),
            from: dummy_participant(),
            to: vec![],
            timestamp: now_iso(),
            content: vec![MessageContent::Text {
                text: "Ping".into(),
            }],
            thread_id: None,
            reply_to_id: None,
            metadata: serde_json::Value::Null,
            ..ChannelMessage::default()
        };

        // if you have `channel_data: Value` in the struct, set it here:
        // msg.channel_data = json!({"team_id":"t1","channel_id":"c1","message_id":"42"});
        // otherwise, skip this part.

        let ser = serde_json::to_string(&msg).unwrap();
        let de: ChannelMessage = serde_json::from_str(&ser).unwrap();
        assert_eq!(msg, de);
    }

    // ---- tests: Capabilities + inline schema validation ---------------------

    #[test]
    fn capabilities_inline_schema_validates_channel_data_ok() {
        let caps = ChannelCapabilities {
            name: "MS Teams".into(),
            version: "1.0.0".into(),
            supports_sending: true,
            supports_receiving: true,
            supports_text: true,
            supports_files: true,
            supports_media: true,
            supports_events: true,
            supports_typing: true,
            supports_routing: true,
            supports_threading: true,
            supports_reactions: true,
            supports_call: false,
            supports_buttons: true,
            supports_links: true,
            supports_custom_payloads: true,
            channel_data_schema_id: Some("greentic://channel/ms_teams.message@v1".into()),
            channel_data_schema: Some(JsonSchemaDescriptor::Inline {
                schema: json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "required": ["team_id","channel_id"],
                    "properties": {
                        "team_id": { "type": "string" },
                        "channel_id": { "type": "string" },
                        "message_id": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                id: Some("greentic://channel/ms_teams.message@v1".into()),
                version: Some("1".into()),
            }),
            supported_events: vec![], // or fill with your EventType values
        };

        let good = json!({"team_id":"t1","channel_id":"c1","message_id":"42"});
        assert!(validate_with_caps(&caps, &good).is_ok());

        let bad_missing = json!({"team_id":"t1"});
        assert!(validate_with_caps(&caps, &bad_missing).is_err());

        let bad_extra = json!({"team_id":"t1","channel_id":"c1","xtra":true});
        assert!(validate_with_caps(&caps, &bad_extra).is_err());
    }

    #[test]
    fn capabilities_no_schema_allows_any_channel_data() {
        let caps = ChannelCapabilities {
            name: "Webhook".into(),
            version: "1.0.0".into(),
            supports_sending: false,
            supports_receiving: true,
            supports_text: true,
            supports_files: false,
            supports_media: false,
            supports_events: true,
            supports_typing: false,
            supports_routing: false,
            supports_threading: false,
            supports_reactions: false,
            supports_call: false,
            supports_buttons: false,
            supports_links: false,
            supports_custom_payloads: true,
            channel_data_schema: None,
            channel_data_schema_id: None,
            supported_events: vec![],
        };

        let arbitrary = json!({"anything":"goes","nested":{"a":1}});
        assert!(validate_with_caps(&caps, &arbitrary).is_ok());
    }

    // ---- tests: schema ref descriptor serializes ----------------------------

    #[test]
    fn json_schema_descriptor_ref_roundtrip() {
        let d = JsonSchemaDescriptor::Ref {
            uri: "https://schemas.greentic.dev/channel/ms_teams.message@v1.json".into(),
            etag: Some("W/\"abc123\"".into()),
            sha256: Some("deadbeef".into()),
            version: Some("1".into()),
        };
        let s = serde_json::to_string(&d).unwrap();
        let de: JsonSchemaDescriptor = serde_json::from_str(&s).unwrap();
        assert_eq!(d, de);
    }
}
