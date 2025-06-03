use async_trait::async_trait;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use ::serde::{Deserialize, Serialize};
use rhai::{Engine, Scope};
use serde_json::json;

use crate::{
    message::Message,
    node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType},
};

/// A Rhai script process node
///
/// This node executes a [Rhai](https://rhai.rs) script to transform message input, payload, and state.
/// It allows complex scripting logic with access to the following variables:
///
/// - `msg`: the entire message object (including id, payload, session_id, etc.)
/// - `payload`: the JSON value of the message payload (auto-parsed if it’s a string)
/// - `state`: the internal node state as a key-value map
/// - Each `state` key is also available as a top-level variable if it's a simple value
///
/// The script must return a value. This will be serialized and wrapped in `{"output": ...}` in the output message.
///
/// ---
///
/// # 🔧 Example 1: Simple arithmetic
///
/// ```rhai
/// let temp = payload["weather"]["temp"];
/// let feels_like = temp - 2;
/// feels_like
/// ```
///
/// Output:
/// ```json
/// { "output": 21 }
/// ```
///
/// ---
///
/// # 📦 Example 2: Returning a structured object
///
/// ```rhai
/// let name = state["user"]["name"];
/// let id = msg.id;
/// #{ greeting: "Hi " + name, id: id }
/// ```
///
/// Output:
/// ```json
/// { "output": { "greeting": "Hi Alice", "id": "abc123" } }
/// ```
///
/// ---
///
/// # 🔁 Example 3: Using conditionals and loops
///
/// ```rhai
/// let sum = 0;
/// for x in payload["values"] {
///     if x > 0 {
///         sum += x;
///     }
/// }
/// sum
/// ```
///
/// Output:
/// ```json
/// { "output": 42 }
/// ```
///
/// ---
///
/// # ✅ Example 4: Working with session ID and setting logic
///
/// ```rhai
/// if msg.session_id == "sess42" {
///     "Session is active"
/// } else {
///     "Unknown session"
/// }
/// ```
///
/// Output:
/// ```json
/// { "output": "Session is active" }
/// ```
///
/// ---
///
/// # 📚 Notes:
///
/// - You can use any function or feature supported by Rhai.
/// - Script errors are caught and returned as `NodeError::InvalidInput`.
/// - All state values are converted to `Dynamic` if possible for easier access.
/// - Avoid mutating external context; the script is intended to be pure and side-effect free.
///
/// ---
///
/// For full language documentation, see: https://rhai.rs/book/
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "script")]
pub struct ScriptProcessNode {
    pub script: String,
}

#[async_trait]
#[typetag::serde]
impl NodeType for ScriptProcessNode {
    fn type_name(&self) -> String {
        "script".to_string()
    }

    fn schema(&self) -> RootSchema {
        schema_for!(ScriptProcessNode)
    }

    #[tracing::instrument(name = "script_node_process", skip(self, context))]
    async fn process(
        &self,
        input: Message,
        context: &mut NodeContext,
    ) -> Result<NodeOut, NodeErr> {
        let engine = Engine::new();
        let mut scope = Scope::new();

        scope.push("msg", input.clone());
        scope.push("state", context.get_all_state().clone());
        scope.push("payload", input.payload().clone());

        for (k, v) in context.get_all_state() {
            if let Ok(json_value) = serde_json::to_value(&v) {
                if let Ok(dynamic) = rhai::serde::to_dynamic(&json_value) {
                    scope.push_dynamic(k, dynamic);
                }
            }
        }

        match engine.eval_with_scope::<rhai::Dynamic>(&mut scope, &self.script) {
            Ok(result) => {
                let dynamic_result = rhai::serde::to_dynamic(result);
                let payload = match dynamic_result {
                    Ok(dynamic) => match rhai::serde::from_dynamic(&dynamic) {
                        Ok(val) => val,
                        Err(_) => json!({"error": "Failed to convert Rhai result to JSON"}),
                    },
                    Err(_) => json!({"error": "Failed to convert Rhai output to Dynamic"}),
                };
                let msg = Message::new(&input.id(), json!({"output": payload}), input.session_id());
                Ok(NodeOut::all(msg))
            },
            Err(err) => Err(NodeErr::all(NodeError::InvalidInput(format!("Script error: {}", err))))
        }
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{message::Message, node::NodeContext};
    use crate::state::StateValue;
    use serde_json::json;

    fn dummy_context_with_state() -> NodeContext {
        let mut ctx = NodeContext::dummy();
        ctx.set_state("age", StateValue::try_from(json!(30)).unwrap());
        ctx.set_state("user", StateValue::try_from(json!({"name": "Alice"})).unwrap());
        ctx
    }

    #[tokio::test]
    async fn test_basic_arithmetic() {
        let node = ScriptProcessNode {
            script: "1 + 2 * 3".into(),
        };

        let msg = Message::new("id", json!({}), None);
        let mut ctx = NodeContext::dummy();

        let output = node.process(msg, &mut ctx).await.unwrap();
        let result = &output.message().payload()["output"];
        assert_eq!(result.as_i64().unwrap(), 7);
    }

    #[tokio::test]
    async fn test_payload_access() {
        let node = ScriptProcessNode {
            script: "payload.weather.temp + 1".into(),
        };

        let msg = Message::new("id", json!({ "weather": { "temp": 20 } }), None);
        let mut ctx = NodeContext::dummy();

        let output = node.process(msg, &mut ctx).await.unwrap();
        let result = &output.message().payload()["output"];
        assert_eq!(result.as_i64().unwrap(), 21);
    }

    #[tokio::test]
    async fn test_accessing_state_directly() {
        let node = ScriptProcessNode {
            script: "age + 5".into(),
        };

        let msg = Message::new("id", json!({}), None);
        let mut ctx = dummy_context_with_state();

        let output = node.process(msg, &mut ctx).await.unwrap();
        let result = &output.message().payload()["output"];
        let number = result.as_f64().unwrap();
        assert_eq!(number, 35.0);
    }

    #[tokio::test]
    async fn test_returning_object() {
        let node = ScriptProcessNode {
            script: r#"
                let name = user.name;
                #{ greeting: "Hello " + name }
            "#.into(),
        };

        let msg = Message::new("id", json!({}), None);
        let mut ctx = dummy_context_with_state();

        let output = node.process(msg, &mut ctx).await.unwrap();
        let result = &output.message().payload()["output"];
        assert_eq!(result["greeting"], "Hello Alice");
    }

    #[tokio::test]
    async fn test_condition_handling() {
        let node = ScriptProcessNode {
            script: r#"
                if msg.session_id() == "abc" {
                    "known"
                } else {
                    "unknown"
                }
            "#.into(),
        };

        let msg = Message::new("id", json!({}), Some("abc".to_string()));

        let mut ctx = NodeContext::dummy();

        let process = node.process(msg, &mut ctx).await;
        println!("out: {:?}",process);
        let output = process.unwrap();
                
        let result = &output.message().payload()["output"];
        assert_eq!(result.as_str().unwrap(), "known");
    }

    #[tokio::test]
    async fn test_script_error_returns_node_error() {
        let node = ScriptProcessNode {
            script: "this_does_not_exist()".into(),
        };

        let msg = Message::new("id", json!({}), None);
        let mut ctx = NodeContext::dummy();

        let result = node.process(msg, &mut ctx).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err().error, NodeError::InvalidInput(_)),
            "Expected InvalidInput error"
        );
    }
}
