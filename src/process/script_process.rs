use async_trait::async_trait;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use ::serde::{Deserialize, Serialize};
use rhai::{serde::to_dynamic, Engine, Scope};
use serde_json::{json, Value};

use crate::{
    flow::state::StateValue, message::Message, node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType, Routing}
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
/// /// ---
///
/// # ⚙️ Advanced Usage: Structured Flow Control via `__greentic`
///
/// In addition to returning a simple value, you can return a structured object under the special `__greentic` key:
///
/// - `payload`: What to send in the output message
/// - `out`: List of next node IDs to call on success
/// - `err`: List of next node IDs to call on error
///
/// ## 🔄 Example 5: Structured Output with Routing
///
/// ```rhai
/// return #{ 
///     __greentic: {
///         payload: #{ confirmed: true, name: state["name"] },
///         out: ["next_node"]
///     } 
/// };
/// ```
///
/// Output:
/// ```json
/// { "confirmed": true, "name": "Alice" }
/// ```
///
/// Routing: → `next_node`
///
/// ---
///
/// ## ⚠️ Example 6: Structured Error Routing
///
/// ```rhai
/// if payload["age"] < 18 {
///     return #{ 
///         __greentic: {
///             payload: #{ error: "User must be over 18" },
///             err: ["error_node"]
///         }
///     };
/// }
/// ```
///
/// This will **trigger a failure** and route to `error_node`.
///
/// ---
///
/// ## 🔁 Example 7: Fallback Without `__greentic`
///
/// If you return a basic value:
///
/// ```rhai
/// "Hi there!"
/// ```
///
/// It will be wrapped like this:
/// ```json
/// { "output": "Hi there!" }
/// ```
///
/// ---
///
/// ## 🧠 State Writes Even on Error
///
/// Even when the script fails, updates to `state` will be committed:
///
/// ```rhai
/// state["attempts"] = 3;
/// throw("something broke");
/// ```
///
/// `"attempts"` will be stored in state even though the node returns an error.
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename = "script")]
pub struct ScriptProcessNode {
    pub script: String,
}

impl ScriptProcessNode{
    pub fn new(script: String) -> Self {
        Self{script}
    }
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
        let state_map: serde_json::Map<String, Value> = context
            .get_all_state()
            .into_iter()
            .map(|(k, v)| (k, v.to_json()))
            .collect();

        if let Ok(dyn_state) = to_dynamic(&Value::Object(state_map)) {
            scope.push_dynamic("state", dyn_state);
        }

        // Convert msg, payload, and state into Dynamic so Rhai can access properties
        if let Ok(dyn_msg) = to_dynamic(&serde_json::to_value(&input).unwrap_or_default()) {
            scope.push_dynamic("msg", dyn_msg);
        }

        if let Ok(dyn_payload) = to_dynamic(&input.payload()) {
            scope.push_dynamic("payload", dyn_payload);
        }

        // let the script have access to all the connections of the node
        if let Ok(dyn_connections) = to_dynamic(&context.connections()) {
            scope.push_dynamic("connections", dyn_connections);
        }


       match engine.eval_with_scope::<rhai::Dynamic>(&mut scope, &self.script) {
            Ok(result) => {
                match result.clone().as_map_ref() {
                    Ok(output_obj) if output_obj.contains_key("__greentic") => {
                        let greentic = output_obj.get("__greentic").unwrap();

                        let gmap = greentic.clone_cast::<rhai::Map>();
                        let out_routing = gmap.get("out")
                            .map(|v| extract_routing_list(v))
                            .unwrap_or_default();

                        let err_routing = gmap.get("err")
                            .map(|v| extract_routing_list(v))
                            .unwrap_or_default();

                        let payload = gmap.get("payload")
                            .and_then(|v| rhai::serde::from_dynamic::<serde_json::Value>(v).ok())
                            .unwrap_or(json!({ "error": "Missing or invalid payload" }));

                        let msg = Message::new(&input.id(), payload.clone(), input.session_id());

                        if !err_routing.is_empty() {
                            return Err(NodeErr::next(
                                NodeError::InvalidInput(serde_json::to_string(&json!({ "error": payload })).expect("bug in script node")),
                                Some(err_routing),
                            ));
                        } else if !out_routing.is_empty() {
                            return Ok(NodeOut::next(msg, Some(out_routing)));
                        } else {
                            return Ok(NodeOut::with_routing(msg, Routing::FollowGraph));
                        }
                    }
                    _ => {
                        // fallback: return raw result wrapped under `output`
                        let payload = match rhai::serde::to_dynamic(result)
                            .and_then(|dyn_val| rhai::serde::from_dynamic(&dyn_val))
                        {
                            Ok(val) => val,
                            Err(_) => json!({ "error": "Failed to convert Rhai result to JSON" }),
                        };

                        let msg = Message::new(&input.id(), json!({ "output": payload }), input.session_id());
                        Ok(NodeOut::with_routing(msg, Routing::FollowGraph))
                    }
                }
            }
            Err(err) => {
                // Read back the updated state and apply it
                if let Some(new_state) = scope.get_value::<rhai::Map>("state") {
                    for (k, v) in new_state {
                        if let Ok(json_value) = rhai::serde::from_dynamic::<serde_json::Value>(&v) {
                            if let Ok(state_val) = StateValue::try_from(json_value) {
                                context.set_state(&k, state_val);
                            }
                        }
                    }
                }
                Err(NodeErr::fail(NodeError::InvalidInput(format!(
                    "Script error: {}", err))))
            },
        }
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

fn extract_routing_list(val: &rhai::Dynamic) -> Vec<String> {
    if let Ok(list) = rhai::serde::from_dynamic::<Vec<String>>(val) {
        list
    } else if let Ok(single) = rhai::serde::from_dynamic::<String>(val) {
        vec![single]
    } else {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{message::Message, node::NodeContext};
    use crate::flow::state::StateValue;
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

        let msg = Message::new("id", json!({}), "123".to_string());
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

        let msg = Message::new("id", json!({ "weather": { "temp": 20 } }), "123".to_string());
        let mut ctx = NodeContext::dummy();

        let output = node.process(msg, &mut ctx).await.unwrap();
        let result = &output.message().payload()["output"];
        assert_eq!(result.as_i64().unwrap(), 21);
    }

    #[tokio::test]
    async fn test_accessing_state_directly() {
        let node = ScriptProcessNode {
            script: "state.age + 5".into(),
        };

        let msg = Message::new("id", json!({}), "123".to_string());
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
                let name = state.user.name;
                #{ greeting: "Hello " + name }
            "#.into(),
        };

        let msg = Message::new("id", json!({}), "123".to_string());
        let mut ctx = dummy_context_with_state();

        let output = node.process(msg, &mut ctx).await.unwrap();
        let result = &output.message().payload()["output"];
        assert_eq!(result["greeting"], "Hello Alice");
    }

    #[tokio::test]
    async fn test_condition_handling() {
        let node = ScriptProcessNode {
            script: r#"
                if msg.session_id == "abc" {
                    "known"
                } else {
                    "unknown"
                }
            "#.into(),
        };

        let msg = Message::new("id", json!({}), "abc".to_string());

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

        let msg = Message::new("id", json!({}),"123".to_string());
        let mut ctx = NodeContext::dummy();

        let result = node.process(msg, &mut ctx).await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err().error(), NodeError::InvalidInput(_)),
            "Expected InvalidInput error"
        );
    }
}
