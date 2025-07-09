//! src/ollama_schema.rs
use schemars::{

    json_schema, JsonSchema, Schema, SchemaGenerator
};
use std::borrow::Cow;

use super::ollama::{OllamaAgent, OllamaMode};

impl JsonSchema for OllamaAgent {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("OllamaAgent")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(format!("{}::OllamaAgent", module_path!()))
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "required": ["task"],
            "properties": {
                "task": { "type": "string" },
                "model": { "type": ["string", "null"] },
                "mode": {
                    "type": ["string", "null"],
                    "enum": ["embed", "chat", "generate", null]
                },
                "ollama_url": { "type": ["string", "null"] },
                "model_options": {
                    "type": ["object", "null"],
                    "additionalProperties": true
                },
                "tool_names": {
                    "type": ["array", "null"],
                    "items": { "type": "string" }
                }
            }
        })
    }

    fn inline_schema() -> bool {
        false
    }
}


impl JsonSchema for OllamaMode {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("OllamaMode")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(format!("{}::OllamaMode", module_path!()))
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "enum": ["embed", "chat", "generate"]
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::{Schema};
    use serde_json::{json, Value};

    #[test]
    fn generates_expected_schema() {
        // Generate in-memory schema
        let mut generate = SchemaGenerator::default();
        let schema: Schema = <OllamaAgent>::json_schema(&mut generate);

        // Smoke-check: the root must be an object with a "task" property
        let as_json: Value = serde_json::to_value(&schema).unwrap();
        let task_type = as_json
            .get("properties")
            .and_then(|p| p.get("task"))
            .and_then(|t| t.get("type"))
            .unwrap();

        assert_eq!(task_type, &json!("string"));
    }
}