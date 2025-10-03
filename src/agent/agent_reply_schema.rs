//! Shared serde/schemars types that describe how agents hand structured
//! responses back to Greentic flows.
//!
//! The [`AgentReply`] enum is the contract LLM-backed nodes must fulfill. Each
//! agent, whether built on OpenAI, Ollama, or something custom, should return a
//! JSON object that deserialises into this type so the runtime can route
//! messages and mutate session state.

use schemars::JsonSchema;

use serde::{Deserialize, Serialize};

use crate::flow::state::StateValue;

/// Structured response returned by an agent node.
///
/// The runtime serialises this enum so LLMs can be instructed to respond with
/// predictable shapes. A minimal success reply looks like:
///
/// ```json
/// {
///   "Success": {
///     "payload": "{\"text\":\"hi\"}",
///     "connections": ["next_node"]
///   }
/// }
/// ```
///
/// For follow-up questions agents must emit the [`NeedMoreInfo`] variant to
/// tell the engine to send a reply back to the originating channel.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
//#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentReply {
    Success {
        payload: String,
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
// ---------------------------------------------------

/// Key/value information the agent wants to persist in the session state.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StateKeyValue {
    pub key: String,
    pub value: String,
    pub value_type: ValueType,
}

// ---------------------------------------------------
/// Declares the runtime data type for a [`StateKeyValue`].
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Integer,
    Number,
    Boolean,
    Array,
}

// ---------------------------------------------------

/// Payload emitted when an agent needs the user to supply additional
/// information.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FollowUpPayload {
    pub text: String,
}

/// Convert a raw string returned by an LLM into a strongly-typed
/// [`StateValue`].
///
/// ```rust
/// use greentic::agent::agent_reply_schema::{parse_state_value, ValueType};
/// use greentic::flow::state::StateValue;
///
/// let parsed = parse_state_value(&ValueType::Integer, "42").unwrap();
/// assert_eq!(parsed, StateValue::Integer(42));
/// ```
///
/// The parser performs minimal validation: malformed integers, floats, or
/// booleans return a descriptive `Err` string so the caller can surface useful
/// diagnostics.
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::state::StateValue;

    #[test]
    fn test_parse_string() {
        let result = parse_state_value(&ValueType::String, "hello").unwrap();
        assert_eq!(result, StateValue::String("hello".to_string()));
    }

    #[test]
    fn test_parse_integer() {
        let result = parse_state_value(&ValueType::Integer, "42").unwrap();
        assert_eq!(result, StateValue::Integer(42));
    }

    #[test]
    fn test_parse_number() {
        let result = parse_state_value(&ValueType::Number, "3.14").unwrap();
        assert_eq!(result, StateValue::Float(3.14));
    }

    #[test]
    fn test_parse_boolean_true() {
        let result = parse_state_value(&ValueType::Boolean, "true").unwrap();
        assert_eq!(result, StateValue::Boolean(true));
    }

    #[test]
    fn test_parse_boolean_false() {
        let result = parse_state_value(&ValueType::Boolean, "false").unwrap();
        assert_eq!(result, StateValue::Boolean(false));
    }

    #[test]
    fn test_parse_boolean_invalid() {
        let result = parse_state_value(&ValueType::Boolean, "yes");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_array() {
        let result = parse_state_value(&ValueType::Array, "a,b,c").unwrap();
        assert_eq!(
            result,
            StateValue::List(vec![
                StateValue::String("a".to_string()),
                StateValue::String("b".to_string()),
                StateValue::String("c".to_string())
            ])
        );
    }
}
