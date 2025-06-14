use std::{collections::HashMap, fmt};
use schemars::{schema::{Schema, SchemaObject}, JsonSchema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use crate::{message::Message, node::NodeError};
#[typetag::serde] 
pub trait TransformerType: Send + Sync {
    fn transform(&self, input: &Message, context: &TransformContext) -> Result<Message, NodeError>;
    fn clone_box(&self) -> Box<dyn TransformerType>;
    fn get_schema(&self) -> schemars::schema::RootSchema;
}

#[derive(Serialize, Deserialize)]
pub struct Transformer(pub Box<dyn TransformerType>);


impl fmt::Debug for Transformer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Transformer({:?})", self)
    }
}

impl Clone for Transformer {
    fn clone(&self) -> Self {
        Transformer(self.0.clone_box())
    }
}

// Optional if you want to access `.0`
impl std::ops::Deref for Transformer {
    type Target = dyn TransformerType;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl std::ops::DerefMut for Transformer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.0
    }
}


/// now give it a JsonSchema impl
impl JsonSchema for Transformer {
    fn schema_name() -> String {
        // Optional: a catch‐all name.
        "transformer".to_string()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        // we cannot inspect the *value* here – but we can use the *type*’s get_schema.
        // since this is a dynamic object, we’ll just produce an *open* schema,
        // or you could merge in all known definitions.
        let root = generator.root_schema_for::<serde_json::Value>();
        Schema::Object(SchemaObject {
            // a loose `anyOf` over all the concrete variants’ schemas:
            subschemas: Some(Box::new(schemars::schema::SubschemaValidation {
                any_of: Some(root.definitions.values().cloned().collect()),
                .. Default::default()
            })),
            .. Default::default()
        })
    }
}


#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TransformContext {
    pub caller: String,
    pub config: HashMap<String, String>,
}


pub struct TransformerRegistry {
    transformers: HashMap<String, Transformer>,
}

impl TransformerRegistry {
    pub fn new() -> Self {
        let transformers = HashMap::new();
        Self { transformers }
    }
    pub fn get(&self, name: &str) -> Option<&Transformer> {
        self.transformers.get(name)
    }
    pub fn add<T: TransformerType + 'static>(&mut self, name: String, transformer: T) {
        self.transformers.insert(name, Transformer(Box::new(transformer)));
    }
    pub fn remove(&mut self, name: &str) {
        self.transformers.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::node::NodeError;
    use schemars::schema::RootSchema;
    use schemars::{schema_for, JsonSchema};
    use serde_json::json;

    #[derive(Serialize, Deserialize, Clone, JsonSchema)]
    struct UppercaseTransformer;

    #[typetag::serde]
    impl TransformerType for UppercaseTransformer {
        fn transform(&self, input: &Message, _context: &TransformContext) -> Result<Message, NodeError> {
            let binding = input.payload();
            let original = binding["text"].as_str().unwrap_or_default();
            let upper = original.to_uppercase();
            Ok(Message::new(input.id().as_str(), json!({ "text": upper }),input.session_id()))
        }
        fn clone_box(&self) -> Box<dyn TransformerType> {
            Box::new(self.clone())
        }
        fn get_schema(&self) -> RootSchema {
            // this is object-safe because it doesn't mention `Self: Sized`
            schema_for!(UppercaseTransformer)
        }
    }

    fn make_context() -> TransformContext {
        TransformContext {
            caller: "tester".into(),
            config: HashMap::from([("lang".into(), "en".into())]),
        }
    }

    #[test]
    fn test_transform_context_serialization() {
        let context = make_context();
        let json = serde_json::to_string(&context).unwrap();
        let deserialized: TransformContext = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.caller, "tester");
        assert_eq!(deserialized.config.get("lang"), Some(&"en".to_string()));
    }

    #[test]
    fn test_registry_add_get_remove() {
        let mut registry = TransformerRegistry::new();
        registry.add("upper".into(), UppercaseTransformer);

        assert!(registry.get("upper").is_some());

        registry.remove("upper");
        assert!(registry.get("upper").is_none());
    }

    #[test]
    fn test_transformer_execution() {
        let transformer = UppercaseTransformer;
        let context = make_context();
        let input = Message::new("123", json!({ "text": "hello" }),"123".to_string());

        let output = transformer.transform(&input, &context).unwrap();
        assert_eq!(output.payload()["text"], "HELLO");
    }
}
