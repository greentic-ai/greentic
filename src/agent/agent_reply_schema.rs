use schemars::JsonSchema;

use serde::{Deserialize, Serialize};

use crate::flow::state::StateValue;

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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StateKeyValue {
    pub key: String,
    pub value: String,
    pub value_type: ValueType,
}

// ---------------------------------------------------
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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FollowUpPayload {
    pub text: String,
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
