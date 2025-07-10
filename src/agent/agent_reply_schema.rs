use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::flow::state::StateValue;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentReply {
    Success {
        payload: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state_add: Option<Vec<StateKeyValue>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state_update: Option<Vec<StateKeyValue>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state_delete: Option<Vec<String>>,
        connections: Vec<String>,
    },
    NeedMoreInfo {
        payload: FollowUpPayload,
    },
}

impl JsonSchema for AgentReply {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("AgentReply")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(format!("{}::AgentReply", module_path!()))
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "title": "AgentReply",
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "type": { "const": "success" },
                        "payload": {},
                        "state_add": {
                            "type": ["array", "null"],
                            "items": { "$ref": "#/$defs/StateKeyValue" }
                        },
                        "state_update": {
                            "type": ["array", "null"],
                            "items": { "$ref": "#/$defs/StateKeyValue" }
                        },
                        "state_delete": {
                            "type": ["array", "null"],
                            "items": { "type": "string" }
                        },
                        "connections": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["type", "payload", "connections"]
                },
                {
                    "type": "object",
                    "properties": {
                        "type": { "const": "need_more_info" },
                        "payload": { "$ref": "#/$defs/FollowUpPayload" }
                    },
                    "required": ["type", "payload"]
                }
            ]
        })
    }

    fn inline_schema() -> bool {
        false
    }
}

// ---------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct StateKeyValue {
    pub key: String,
    pub value: String,
    pub value_type: ValueType,
}

impl JsonSchema for StateKeyValue {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("StateKeyValue")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(format!("{}::StateKeyValue", module_path!()))
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "key": { "type": "string" },
                "value": { "type": "string" },
                "value_type": { "$ref": "#/$defs/ValueType" }
            },
            "required": ["key", "value", "value_type"]
        })
    }

    fn inline_schema() -> bool {
        false
    }
}

// ---------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Integer,
    Number,
    Boolean,
    Array,
}

impl JsonSchema for ValueType {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ValueType")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(format!("{}::ValueType", module_path!()))
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "enum": ["string", "integer", "number", "boolean", "array"]
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

// ---------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct FollowUpPayload {
    pub text: String,
}

impl JsonSchema for FollowUpPayload {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("FollowUpPayload")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(format!("{}::FollowUpPayload", module_path!()))
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "text": { "type": "string" }
            },
            "required": ["text"]
        })
    }

    fn inline_schema() -> bool {
        false
    }
}


pub fn parse_state_value(value_type: &ValueType, raw: &str) -> Result<StateValue, String> {
    match value_type {
        ValueType::String => Ok(StateValue::String(raw.to_string())),

        ValueType::Integer => raw
            .parse::<i64>()
            .map(StateValue::Integer)
            .map_err(|e| format!("Invalid integer: {}", e)),

        ValueType::Number => raw
            .parse::<f64>()
            .map(StateValue::Float)
            .map_err(|e| format!("Invalid number: {}", e)),

        ValueType::Boolean => match raw.trim().to_lowercase().as_str() {
            "true" => Ok(StateValue::Boolean(true)),
            "false" => Ok(StateValue::Boolean(false)),
            _ => Err("Invalid boolean: must be 'true' or 'false'".to_string()),
        },

        ValueType::Array => {
            // Basic comma-separated string parsing: e.g., "a, b, c"
            let values = raw
                .split(',')
                .map(|s| StateValue::String(s.trim().to_string()))
                .collect::<Vec<_>>();
            Ok(StateValue::List(values))
        }
    }
}

