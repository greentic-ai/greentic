//! src/ollama_schema.rs
use schemars::{

    JsonSchema, SchemaGenerator,
};
use serde_json::json;
use std::{borrow::Cow, collections::BTreeSet};

use super::ollama::{OllamaAgent, OllamaMode};

impl JsonSchema for OllamaAgent {
    fn schema_name() -> String {
        "OllamaAgent".into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        // helper: string | null
        let string_or_null = SchemaObject {
            instance_type: Some(SingleOrVec::Vec(vec![
                InstanceType::String,
                InstanceType::Null,
            ])),
            ..Default::default()
        };

        // helper: object | null  (open-ended additionalProperties=true)
        let object_or_null = SchemaObject {
            instance_type: Some(SingleOrVec::Vec(vec![
                InstanceType::Object,
                InstanceType::Null,
            ])),
            object: Some(Box::new(ObjectValidation {
                additional_properties: Some(Box::new(Schema::Bool(true))),
                ..Default::default()
            })),
            ..Default::default()
        };

        let mut props: Map<String, Schema> = Map::new();

        // ── task (required string) ────────────────────────────────────────
        props.insert(
            "task".into(),
            Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
                ..Default::default()
            }),
        );

        // model (string|null) ---------------------------------------------
        props.insert("model".into(), Schema::Object(string_or_null.clone()));

        // mode (enum|string|null) -----------------------------------------
        props.insert(
            "mode".into(),
            Schema::Object(SchemaObject {
                enum_values: Some(vec![
                    json!("embed"),
                    json!("chat"),
                    json!("generate"),
                ]),
                instance_type: Some(SingleOrVec::Vec(vec![
                    InstanceType::String,
                    InstanceType::Null,
                ])),
                ..Default::default()
            }),
        );

        // ollama_host (string|null) ---------------------------------------
        props.insert("ollama_host".into(), Schema::Object(string_or_null.clone()));

        // ollama_port (integer|null) --------------------------------------
        props.insert(
            "ollama_port".into(),
            Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Vec(vec![
                    InstanceType::Integer,
                    InstanceType::Null,
                ])),
                ..Default::default()
            }),
        );

        // model_options (object|null) -------------------------------------
        props.insert("model_options".into(), Schema::Object(object_or_null.clone()));

        // tool_names (array<string>|null) ---------------------------------
        let array_of_str = SchemaObject {
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Array))),
            array: Some(Box::new(ArrayValidation {
                items: Some(SingleOrVec::Single(Box::new(Schema::Object(
                    SchemaObject {
                        instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
                        ..Default::default()
                    },
                )))),
                ..Default::default()
            })),
            ..Default::default()
        };
        let array_or_null = SchemaObject {
            instance_type: Some(SingleOrVec::Vec(vec![
                InstanceType::Array,
                InstanceType::Null,
            ])),
            array: array_of_str.array.clone(),
            ..array_of_str
        };
        props.insert("tool_names".into(), Schema::Object(array_or_null));

        // ── wrap everything into an object schema ─────────────────────────
        let mut validation = ObjectValidation::default();
        validation.properties = props;
        validation.required = {
            let mut s = BTreeSet::new();
            s.insert("task".into());
            s
        };

        Schema::Object(SchemaObject {
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
            object: Some(Box::new(validation)),
            ..Default::default()
        })
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(Self::schema_name())
    }
    
    fn inline_schema() -> bool {
        false
    }
}

impl JsonSchema for OllamaMode {
    fn schema_name() -> String {
        "OllamaMode".into()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        Schema::Object(SchemaObject {
            enum_values: Some(vec![
                json!("embed"),
                json!("chat"),
                json!("generate"),
            ]),
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::{Schema};
    use serde_json::Value;

    #[test]
    fn generates_expected_schema() {
        // Generate in-memory schema
        let mut generate = SchemaGenerator::default();
        let schema: Schema = <OllamaAgent>::json_schema(&mut generate);

        // Smoke-check: the root must be an object with a "task" property
        let as_json: Value = serde_json::to_value(&schema).unwrap();
        let task_type = &as_json
            .get("properties")
            .and_then(|p| p.get("task"))
            .and_then(|t| t.get("type"))
            .unwrap();

        assert_eq!(task_type, "string");
    }
}