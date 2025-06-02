// src/json_schema_impls.rs

use schemars::schema::{
    ArrayValidation,           // ← needed for array schemas
    InstanceType,
    Metadata,
    ObjectValidation,
    Schema,
    SchemaObject,
    SingleOrVec,
    SubschemaValidation,
};
    // (only if you still need `schema_for!(…)` elsewhere)
use schemars::{JsonSchema, SchemaGenerator};
use serde_json::json;
use std::collections::BTreeSet;

use super::ollama::{OllamaAgent, OllamaRequest};



// -------------------------------------------------------------------------
// 1) OllamaAgent’s JsonSchema
// -------------------------------------------------------------------------

impl JsonSchema for OllamaAgent {
    fn schema_name() -> String {
        "OllamaAgent".to_owned()
    }

    fn json_schema(_gen: &mut SchemaGenerator) -> Schema {
        // — “string or null”
        let string_or_null = SchemaObject {
            instance_type: Some(SingleOrVec::Vec(vec![
                InstanceType::String,
                InstanceType::Null,
            ])),
            ..Default::default()
        };

        // — “integer or null”
        let int_or_null = SchemaObject {
            instance_type: Some(SingleOrVec::Vec(vec![
                InstanceType::Integer,
                InstanceType::Null,
            ])),
            ..Default::default()
        };

        // — “generic object or null” (for model_options)
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

        // Build the “properties” map:
        let mut props = schemars::Map::new();

        // • model: { type: "string" }
        props.insert(
            "model".to_string(),
            Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::String,
                ))),
                ..Default::default()
            }),
        );

        // • ollama_host: Option<Url> → “string or null”
        props.insert("ollama_host".to_string(), Schema::Object(string_or_null.clone()));

        // • ollama_port: Option<u16> → “integer or null”
        //   (we could attach min/max in metadata if desired, but it’s optional)
        let mut port_schema = int_or_null.clone();
        if port_schema.metadata.is_none() {
            port_schema.metadata = Some(Box::new(Metadata::default()));
        }
        props.insert("ollama_port".to_string(), Schema::Object(port_schema));

        // • model_options: Option<…> → “object or null”
        props.insert("model_options".to_string(), Schema::Object(object_or_null.clone()));

        // Finally assemble the root object schema:
        let mut validation = ObjectValidation::default();
        validation.properties = props;

        // “model” is required; ollama_host/ollama_port/model_options are optional
        let mut required_set = BTreeSet::new();
        required_set.insert("model".to_string());
        validation.required = required_set;

        let schema_obj = SchemaObject {
            instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
            object: Some(Box::new(validation)),
            ..Default::default()
        };

        Schema::Object(schema_obj)
    }
    
    fn is_referenceable() -> bool {
        true
    }
    
    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(Self::schema_name())
    }
}


// -------------------------------------------------------------------------
// 2) OllamaRequest’s JsonSchema
// -------------------------------------------------------------------------

impl JsonSchema for OllamaRequest {
    fn schema_name() -> String {
        "OllamaRequest".to_owned()
    }

    fn json_schema(_generate: &mut SchemaGenerator) -> Schema {
        // We will collect one SchemaObject per variant under “anyOf”:
        let mut any_of_schemas = Vec::new();

        // --- 1) ToolCall variant ---
        {
            let mut tool_props = schemars::Map::new();

            // “mode”: { "const": "tool_call" }
            tool_props.insert(
                "mode".to_string(),
                Schema::Object(SchemaObject {
                    enum_values: Some(vec![json!("tool_call")]),
                    ..Default::default()
                }),
            );

            // “tool_node”: opaque JSON object
            let any_object = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
                object: Some(Box::new(ObjectValidation {
                    additional_properties: Some(Box::new(Schema::Bool(true))),
                    ..Default::default()
                })),
                ..Default::default()
            });
            tool_props.insert("tool_node".to_string(), any_object.clone());

            // “input”: opaque JSON object
            tool_props.insert("input".to_string(), any_object.clone());

            // “msg”: opaque JSON object
            tool_props.insert("msg".to_string(), any_object.clone());

            // Build ObjectValidation for ToolCall:
            let mut tool_validation = ObjectValidation::default();
            tool_validation.properties = tool_props;

            // Required fields for ToolCall: “mode”, “tool_node”, “input”, “msg”
            let mut tool_req = BTreeSet::new();
            tool_req.insert("mode".to_string());
            tool_req.insert("tool_node".to_string());
            tool_req.insert("input".to_string());
            tool_req.insert("msg".to_string());
            tool_validation.required = tool_req;

            let tool_schema_obj = SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
                object: Some(Box::new(tool_validation)),
                ..Default::default()
            };
            any_of_schemas.push(Schema::Object(tool_schema_obj));
        }

        // --- 2) Embed variant ---
        {
            let mut embed_props = schemars::Map::new();

            // “mode”: { "const": "embed" }
            embed_props.insert(
                "mode".to_string(),
                Schema::Object(SchemaObject {
                    enum_values: Some(vec![json!("embed")]),
                    ..Default::default()
                }),
            );

            // “model”: { type: "string" }
            embed_props.insert(
                "model".to_string(),
                Schema::Object(SchemaObject {
                    instance_type: Some(SingleOrVec::Single(Box::new(
                        InstanceType::String,
                    ))),
                    ..Default::default()
                }),
            );

            // “text”: { type: "string" }
            embed_props.insert(
                "text".to_string(),
                Schema::Object(SchemaObject {
                    instance_type: Some(SingleOrVec::Single(Box::new(
                        InstanceType::String,
                    ))),
                    ..Default::default()
                }),
            );

            let mut embed_validation = ObjectValidation::default();
            embed_validation.properties = embed_props;

            // Required for Embed: “mode”, “model”, “text”
            let mut embed_req = BTreeSet::new();
            embed_req.insert("mode".to_string());
            embed_req.insert("model".to_string());
            embed_req.insert("text".to_string());
            embed_validation.required = embed_req;

            let embed_schema_obj = SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Object))),
                object: Some(Box::new(embed_validation)),
                ..Default::default()
            };
            any_of_schemas.push(Schema::Object(embed_schema_obj));
        }

        // --- 3) Chat variant ---
        {
            let mut chat_props = schemars::Map::new();

            // “mode”: { "const": "chat" }
            chat_props.insert(
                "mode".to_string(),
                Schema::Object(SchemaObject {
                    enum_values: Some(vec![json!("chat")]),
                    ..Default::default()
                }),
            );

            // “model”: { type: "string" }
            chat_props.insert(
                "model".to_string(),
                Schema::Object(SchemaObject {
                    instance_type: Some(SingleOrVec::Single(Box::new(
                        InstanceType::String,
                    ))),
                    ..Default::default()
                }),
            );

            // Manually build “history”: array of ChatMessage
            //
            // ChatMessage has these fields:
            // - role: MessageRole  → enum of ["user","assistant","system","tool"]
            // - content: String
            // - tool_calls: Vec<ToolCall>  → array of opaque objects
            // - images: Option<Vec<Image>> → nullable array of opaque objects

            // (1) role enum
            let mut role_enum_values = Vec::new();
            role_enum_values.push(json!("user"));
            role_enum_values.push(json!("assistant"));
            role_enum_values.push(json!("system"));
            role_enum_values.push(json!("tool"));
            let role_schema = Schema::Object(SchemaObject {
                enum_values: Some(role_enum_values),
                ..Default::default()
            });

            // (2) content string
            let content_schema = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::String,
                ))),
                ..Default::default()
            });

            // (3) tool_calls: array of opaque objects
            let any_object = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::Object,
                ))),
                object: Some(Box::new(ObjectValidation {
                    additional_properties: Some(Box::new(Schema::Bool(true))),
                    ..Default::default()
                })),
                ..Default::default()
            });
            let tool_calls_item = any_object.clone();
            let tool_calls_schema = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::Array,
                ))),
                array: Some(Box::new(ArrayValidation {
                    items: Some(SingleOrVec::Single(Box::new(tool_calls_item))),
                    ..Default::default()
                })),
                ..Default::default()
            });

            // (4) images: Option<Vec<Image>> → nullable array of opaque objects
            let image_item = any_object.clone();
            let _images_array = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::Array,
                ))),
                array: Some(Box::new(ArrayValidation {
                    items: Some(SingleOrVec::Single(Box::new(image_item.clone()))),
                    ..Default::default()
                })),
                ..Default::default()
            });
            let images_schema = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Vec(vec![
                    InstanceType::Array,
                    InstanceType::Null,
                ])),
                // We allow either array‐of‐opaque or null
                array: Some(Box::new(ArrayValidation {
                    items: Some(SingleOrVec::Single(Box::new(image_item.clone()))),
                    ..Default::default()
                })),
                object: Some(Box::new(ObjectValidation {
                    additional_properties: None,
                    ..Default::default()
                })),
                subschemas: Some(Box::new(SubschemaValidation {
                    // no extra “oneOf"/"allOf"; just array ∪ null
                    ..Default::default()
                })),
                ..Default::default()
            });

            // Now assemble the ChatMessage item‐object:
            let mut chat_item_props = schemars::Map::new();
            chat_item_props.insert("role".to_string(), role_schema.clone());
            chat_item_props.insert("content".to_string(), content_schema.clone());
            chat_item_props.insert("tool_calls".to_string(), tool_calls_schema.clone());
            chat_item_props.insert("images".to_string(), images_schema.clone());

            // Required: “role” and “content”
            let mut chat_item_req = BTreeSet::new();
            chat_item_req.insert("role".to_string());
            chat_item_req.insert("content".to_string());

            let chat_item_validation = ObjectValidation {
                properties: chat_item_props,
                required: chat_item_req,
                additional_properties: Some(Box::new(Schema::Bool(false))),
                ..Default::default()
            };

            let chat_item_schema = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::Object,
                ))),
                object: Some(Box::new(chat_item_validation)),
                ..Default::default()
            });

            // Wrap that in an array → history: Vec<ChatMessage>
            let history_schema = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::Array,
                ))),
                array: Some(Box::new(ArrayValidation {
                    items: Some(SingleOrVec::Single(Box::new(chat_item_schema.clone()))),
                    ..Default::default()
                })),
                ..Default::default()
            });
            chat_props.insert("history".to_string(), history_schema.clone());

            // “model_options”: Option<…> again as object or null
            let object_or_null = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Vec(vec![
                    InstanceType::Object,
                    InstanceType::Null,
                ])),
                object: Some(Box::new(ObjectValidation {
                    additional_properties: Some(Box::new(Schema::Bool(true))),
                    ..Default::default()
                })),
                ..Default::default()
            });
            chat_props.insert("model_options".to_string(), Schema::Object(object_or_null.clone().into()));

            // Assemble the “Chat” variant schema entry
            let mut chat_validation = ObjectValidation::default();
            chat_validation.properties = chat_props;

            // Required: “mode”, “model”, “history”
            let mut chat_req = BTreeSet::new();
            chat_req.insert("mode".to_string());
            chat_req.insert("model".to_string());
            chat_req.insert("history".to_string());
            chat_validation.required = chat_req;

            let chat_schema_obj = SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::Object,
                ))),
                object: Some(Box::new(chat_validation)),
                ..Default::default()
            };
            any_of_schemas.push(Schema::Object(chat_schema_obj));
        }

        // --- 4) Generate variant ---
        {
            let mut gen_props: schemars::Map<String, Schema> = schemars::Map::new();
            // “mode”: { "const": "generate" }
            gen_props.insert(
                "mode".to_string(),
                Schema::Object(SchemaObject {
                    enum_values: Some(vec![json!("generate")]),
                    ..Default::default()
                }),
            );

            // “model”: { type: "string" }
            gen_props.insert(
                "model".to_string(),
                Schema::Object(SchemaObject {
                    instance_type: Some(SingleOrVec::Single(Box::new(
                        InstanceType::String,
                    ))),
                    ..Default::default()
                }),
            );

            // “prompt”: { type: "string" }
            gen_props.insert(
                "prompt".to_string(),
                Schema::Object(SchemaObject {
                    instance_type: Some(SingleOrVec::Single(Box::new(
                        InstanceType::String,
                    ))),
                    ..Default::default()
                }),
            );

            // “model_options”: nullable object
            let object_or_null = Schema::Object(SchemaObject {
                instance_type: Some(SingleOrVec::Vec(vec![
                    InstanceType::Object,
                    InstanceType::Null,
                ])),
                object: Some(Box::new(ObjectValidation {
                    additional_properties: Some(Box::new(Schema::Bool(true))),
                    ..Default::default()
                })),
                ..Default::default()
            });
            gen_props.insert("model_options".to_string(), Schema::Object(object_or_null.clone().into()));

            let mut gen_validation = ObjectValidation::default();
            gen_validation.properties = gen_props;

            // Required: “mode”, “model”, “prompt”
            let mut gen_req = BTreeSet::new();
            gen_req.insert("mode".to_string());
            gen_req.insert("model".to_string());
            gen_req.insert("prompt".to_string());
            gen_validation.required = gen_req;

            let gen_schema_obj = SchemaObject {
                instance_type: Some(SingleOrVec::Single(Box::new(
                    InstanceType::Object,
                ))),
                object: Some(Box::new(gen_validation)),
                ..Default::default()
            };
            any_of_schemas.push(Schema::Object(gen_schema_obj));
        }

        // Combine all variant schemas under a single “anyOf”
        let mut root_obj = SchemaObject::default();
        root_obj.subschemas = Some(Box::new(SubschemaValidation {
            any_of: Some(any_of_schemas),
            ..Default::default()
        }));

        Schema::Object(root_obj)
    }
}
