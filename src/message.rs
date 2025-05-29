use std::collections::HashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
pub struct Message {
    id: String,
    session_id: Option<String>,
    payload: Value,
    metadata: HashMap<String, String>,
}

impl Message {
    pub fn new(id: &str, payload: Value, session_id: Option<String>) -> Self {
        Self {
            id: id.to_string(),
            session_id,
            payload,
            metadata: HashMap::new(),
        }
    }

    pub fn from_error(error: String) -> Self {
        let mut metadata = HashMap::new();
        metadata.insert("error".to_string(), error.clone());

        Self {
            id: uuid::Uuid::new_v4().to_string(), // Requires the `uuid` crate
            session_id: None,
            payload: json!({ "error": error }),
            metadata,
        }
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }

    pub fn session_id(&self) -> Option<String> {
        self.session_id.clone()
    }

    pub fn set_session_id(&mut self, session: Option<String>) {
        self.session_id = session;
    }

    pub fn payload(&self) -> Value {
        self.payload.clone()
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.metadata.get(name)
    }

    pub fn add(&mut self, name: String, value:String ) {
        self.metadata.insert(name, value);
    }

    pub fn remove(&mut self, name: &str ) {
        self.metadata.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_message_creation() {
        let msg = Message::new("abc123", json!({"key": "value"}),None);
        assert_eq!(msg.id(), "abc123");
        assert_eq!(msg.payload(), json!({"key": "value"}));
        assert!(msg.metadata.is_empty());
    }

    #[test]
    fn test_add_and_get_metadata() {
        let mut msg = Message::new("id", json!(null),None);
        msg.add("foo".to_string(), "bar".to_string());

        assert_eq!(msg.get("foo"), Some(&"bar".to_string()));
        assert_eq!(msg.get("missing"), None);
    }

    #[test]
    fn test_remove_metadata() {
        let mut msg = Message::new("id", json!(null),None);
        msg.add("to_remove".to_string(), "bye".to_string());

        assert!(msg.get("to_remove").is_some());
        msg.remove("to_remove");
        assert!(msg.get("to_remove").is_none());
    }

    #[test]
    fn test_metadata_overwrite() {
        let mut msg = Message::new("id", json!(null),None);
        msg.add("key".to_string(), "first".to_string());
        msg.add("key".to_string(), "second".to_string());

        assert_eq!(msg.get("key"), Some(&"second".to_string()));
    }
}
