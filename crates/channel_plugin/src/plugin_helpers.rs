//! Helper functions to construct `ChannelMessage`s, `Event`s and Protobuf-compatible JSON

use chrono::Utc;
use dotenvy::{dotenv_iter, from_path_iter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, to_value as to_json};
//use serde_json::{json, Value};
use crate::jsonrpc::{Id, Request};
use crate::message::*;
use crate::plugin_actor::Method;
use thiserror::Error;
use uuid::Uuid;

// -----------------------------------------------------------------------------
// Event builders
// -----------------------------------------------------------------------------

pub const USER_JOINED: &str = "UserJoined";
pub const USER_LEFT: &str = "UserLeft";

pub fn build_user_joined_event(
    channel: &str,
    user_id: &str,
    session_id: Option<String>,
) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        channel: channel.into(),
        direction: MessageDirection::Incoming,
        session_id,
        from: Participant {
            id: user_id.into(),
            display_name: None,
            channel_specific_id: None,
        },
        to: vec![],
        thread_id: None,
        reply_to_id: None,
        content: vec![MessageContent::Event {
            event: Event {
                event_type: USER_JOINED.to_string(),
                event_payload: json!({ "user_id": user_id }),
            },
        }],
        metadata: Value::Null,
    }
}

pub fn build_user_left_event(
    channel: &str,
    user_id: &str,
    session_id: Option<String>,
) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        direction: MessageDirection::Incoming,
        channel: channel.into(),
        session_id,
        from: Participant {
            id: user_id.into(),
            display_name: None,
            channel_specific_id: None,
        },
        to: vec![],
        thread_id: None,
        reply_to_id: None,
        content: vec![MessageContent::Event {
            event: Event {
                event_type: USER_LEFT.to_string(),
                event_payload: json!({ "user_id": user_id }),
            },
        }],
        metadata: Value::Null,
    }
}

pub fn get_user_joined_left_events() -> Vec<EventType> {
    vec![
        EventType {
            event_type: USER_JOINED.to_string(),
            description: "Event sent when a user connects".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "properties": {
                    "user_id": {
                        "type": "string",
                        "format": "channel specific unique user_id",
                        "description": "A channel specific and potentially only session unique user_id"
                    }
                },
                "required": ["user_id"]
            })),
        },
        EventType {
            event_type: USER_LEFT.to_string(),
            description: "Event sent when a user disconnects".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "properties": {
                    "user_id": {
                        "type": "string",
                        "format": "channel specific unique user_id",
                        "description": "A channel specific and potentially only session unique user_id"
                    }
                },
                "required": ["user_id"]
            })),
        },
    ]
}

/// Build a `ChannelMessage` containing a single text content item.
///
/// Most optional fields are left `None`/defaults to keep the helper light.
pub fn build_text_message(
    from: &str,
    session_id: Option<String>,
    channel: &str,
    text: &str,
) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4().to_string(),
        session_id,
        channel: channel.to_string(),
        direction: MessageDirection::Incoming,
        from: Participant {
            id: from.to_string(),
            display_name: None,
            channel_specific_id: None,
        },
        to: Vec::new(),
        timestamp: Utc::now().to_rfc3339(),
        content: vec![MessageContent::Text {
            text: text.to_string(),
        }],
        thread_id: None,
        reply_to_id: None,
        metadata: serde_json::Value::Null,
    }
}

pub fn build_receive_text_msg(
    from: &str,
    session_id: Option<String>,
    channel: &str,
    text: &str,
) -> MessageInResult {
    MessageInResult {
        message: build_text_message(from, session_id, channel, text),
        error: false,
    }
}
/// Produce a ready-to-print JSON-RPC `messageIn` request.
///
/// `request_id` can be any string or integer converted to a string.
pub fn build_text_response<S: Into<String>>(
    request_id: S,
    from: &str,
    session_id: Option<String>,
    channel: &str,
    text: &str,
) -> Request {
    let params = build_receive_text_msg(from, session_id, channel, text);
    Request::call(
        Id::String(request_id.into()),
        Method::MessageIn,
        Some(to_json(params).expect("serialise params")),
    )
}

/// Read a .env-style file and return two vectors:
///   * `config`  – all keys that **don't** start with `SECRET_`
///   * `secrets` – keys starting with `SECRET_` (prefix stripped)
pub fn load_env_as_vecs(
    secrets_path: Option<&str>,
    config_path: Option<&str>,
) -> anyhow::Result<(Vec<(String, String)>, Vec<(String, String)>)> {
    let mut config = Vec::new();
    let mut secrets = Vec::new();
    if secrets_path.is_some() {
        let secrets_iter = match secrets_path {
            Some(p) => from_path_iter(p)?,
            None => dotenv_iter()?, // default: .env in CWD
        };
        for kv in secrets_iter {
            let (k, v) = kv?;
            secrets.push((k, v));
        }
    }
    if config_path.is_some() {
        let config_iter = match config_path {
            Some(p) => from_path_iter(p)?,
            None => dotenv_iter()?, // default: .env in CWD
        };

        for kv in config_iter {
            let (k, v) = kv?;
            config.push((k, v));
        }
    }
    Ok((config, secrets))
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
