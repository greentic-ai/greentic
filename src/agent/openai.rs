use crate::agent::agent_reply_schema::{AgentReply, FollowUpPayload, parse_state_value};
use crate::flow::state::StateValue;
use crate::message::Message;
use crate::node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType};
use async_trait::async_trait;
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::error;

/// `OpenAiAgent` calls the OpenAI Chat Completions API to produce
/// structured `AgentReply` responses.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct OpenAiAgent {
    /// Task or instruction to execute for every request.
    pub task: String,
    /// Optional system prompt to override the default instructions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Model identifier (defaults to `gpt-4o-mini`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether to include the raw payload in the user message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_payload: Option<bool>,
}

impl Default for OpenAiAgent {
    fn default() -> Self {
        Self {
            task: "Respond to the user".into(),
            system_prompt: None,
            model: None,
            use_payload: Some(true),
        }
    }
}

#[async_trait]
#[typetag::serde]
impl NodeType for OpenAiAgent {
    fn type_name(&self) -> String {
        "openai".to_string()
    }

    fn schema(&self) -> schemars::Schema {
        schemars::schema_for!(OpenAiAgent)
    }

    #[tracing::instrument(name = "openai_agent_node_process", skip(self, context))]
    async fn process(&self, input: Message, context: &mut NodeContext) -> Result<NodeOut, NodeErr> {
        let api_key = context.reveal_secret("OPENAI_KEY").await.ok_or_else(|| {
            NodeErr::fail(NodeError::ExecutionFailed(
                "OPENAI_KEY secret is missing".to_string(),
            ))
        })?;

        let base_url = context
            .get_config("OPENAI_URL")
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        let client = Client::new();

        let payload = input_payload(self.use_payload.unwrap_or(true), &input);
        let state_snapshot = context.get_all_state();
        let connections = context.connections().unwrap_or_default();

        let system_prompt = self.system_prompt.clone().unwrap_or_else(|| {
            r#"You are a structured AI agent inside an automation platform.
Return valid JSON matching the `AgentReply` schema. Never guess values."#
                .into()
        });

        let user_msg = build_user_message(&self.task, payload, state_snapshot, &connections);

        let schema = schemars::schema_for!(AgentReply);
        let schema_json = serde_json::to_value(schema).unwrap_or(json!({"type": "object"}));

        let body = json!({
            "model": self.model.clone().unwrap_or_else(|| "gpt-4o-mini".into()),
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_msg},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "agent_reply",
                    "schema": schema_json
                }
            }
        });

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let resp = client
            .post(url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                NodeErr::fail(NodeError::ExecutionFailed(format!(
                    "OpenAI request failed: {e}"
                )))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_else(|_| "<no body>".into());
            error!("OpenAI error: {}", text);
            return Err(NodeErr::fail(NodeError::ExecutionFailed(format!(
                "OpenAI API returned error: {}",
                text
            ))));
        }

        let json: Value = resp.json().await.map_err(|e| {
            NodeErr::fail(NodeError::ExecutionFailed(format!(
                "Invalid OpenAI response: {e}"
            )))
        })?;

        let choice_content = json
            .pointer("/choices/0/message/content")
            .cloned()
            .ok_or_else(|| {
                NodeErr::fail(NodeError::ExecutionFailed(
                    "OpenAI response missing message content".into(),
                ))
            })?;

        let content_str = match choice_content {
            Value::String(s) => s,
            Value::Array(arr) => choice_array_to_string(arr),
            other => other.to_string(),
        };
        let reply: AgentReply = serde_json::from_str(&content_str).map_err(|e| {
            error!("Failed to parse AgentReply: {}", e);
            NodeErr::fail(NodeError::ExecutionFailed(format!(
                "Failed to parse AgentReply JSON: {e}"
            )))
        })?;

        apply_agent_reply(reply, &input, context)
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

fn input_payload(use_payload: bool, message: &Message) -> Value {
    if use_payload {
        message.payload()
    } else {
        Value::Null
    }
}

fn build_user_message(
    task: &str,
    payload: Value,
    state_snapshot: Vec<(String, StateValue)>,
    connections: &[String],
) -> String {
    let state_json = state_snapshot
        .into_iter()
        .map(|(key, value)| (key, value.to_json()))
        .collect::<serde_json::Map<_, _>>();

    json!({
        "task": task,
        "payload": payload,
        "state": Value::Object(state_json),
        "connections": connections,
    })
    .to_string()
}

fn choice_array_to_string(parts: Vec<Value>) -> String {
    parts
        .into_iter()
        .filter_map(|p| match p {
            Value::Object(mut obj) => obj.remove("text"),
            Value::String(s) => Some(Value::String(s)),
            _ => None,
        })
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

trait AgentReplyContext {
    fn set_state_value(&mut self, key: &str, value: StateValue);
    fn delete_state_key(&mut self, key: &str);
    fn available_connections(&self) -> Option<Vec<String>>;
}

impl AgentReplyContext for NodeContext {
    fn set_state_value(&mut self, key: &str, value: StateValue) {
        self.set_state(key, value);
    }

    fn delete_state_key(&mut self, key: &str) {
        self.delete_state(key);
    }

    fn available_connections(&self) -> Option<Vec<String>> {
        self.connections()
    }
}

fn apply_agent_reply(
    reply: AgentReply,
    input: &Message,
    context: &mut impl AgentReplyContext,
) -> Result<NodeOut, NodeErr> {
    match reply {
        AgentReply::Success {
            payload,
            state_add,
            state_update,
            state_delete,
            connections,
        } => {
            if let Some(states_add) = state_add {
                for state in states_add.iter() {
                    let value = parse_state_value(&state.value_type, &state.value)
                        .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(e)))?;
                    context.set_state_value(&state.key, value);
                }
            }

            if let Some(update) = state_update {
                for state in update.iter() {
                    let value = parse_state_value(&state.value_type, &state.value)
                        .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(e)))?;
                    context.set_state_value(&state.key, value);
                }
            }

            if let Some(delete) = state_delete {
                for key in delete.iter() {
                    context.delete_state_key(key);
                }
            }

            let parsed_payload = match serde_json::from_str(&payload) {
                Ok(value) => value,
                Err(_) => json!({"text": payload.clone()}),
            };

            let message = Message::new(&input.id(), parsed_payload, input.session_id());
            let preferred = match connections.is_empty() {
                true => context.available_connections(),
                false => Some(connections),
            };

            Ok(NodeOut::next(message, preferred))
        }
        AgentReply::NeedMoreInfo { payload } => {
            Ok(NodeOut::reply(follow_up_message(&payload, input)))
        }
    }
}

fn follow_up_message(payload: &FollowUpPayload, input: &Message) -> Message {
    Message::new(
        &input.id(),
        json!({"text": payload.text.clone()}),
        input.session_id(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agent_reply_schema::{StateKeyValue, ValueType};
    use serde_json::json;
    use std::collections::HashMap;

    struct TestContext {
        state: HashMap<String, StateValue>,
        connections: Option<Vec<String>>,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                state: HashMap::new(),
                connections: None,
            }
        }

        fn with_connections(self, connections: Vec<String>) -> Self {
            Self {
                connections: Some(connections),
                ..self
            }
        }
    }

    impl AgentReplyContext for TestContext {
        fn set_state_value(&mut self, key: &str, value: StateValue) {
            self.state.insert(key.to_string(), value);
        }

        fn delete_state_key(&mut self, key: &str) {
            self.state.remove(key);
        }

        fn available_connections(&self) -> Option<Vec<String>> {
            self.connections.clone()
        }
    }

    fn sample_message() -> Message {
        Message::new("msg", json!({"text": "hi"}), "session".into())
    }

    #[test]
    fn build_user_message_honors_payload_flag() {
        let msg = sample_message();
        let included = build_user_message(
            "task",
            input_payload(true, &msg),
            vec![("foo".into(), StateValue::String("bar".into()))],
            &[],
        );
        assert!(included.contains("\"payload\":{\"text\":\"hi\"}"));

        let excluded = build_user_message(
            "task",
            input_payload(false, &msg),
            vec![("foo".into(), StateValue::String("bar".into()))],
            &[],
        );
        assert!(excluded.contains("\"payload\":null"));
    }

    #[test]
    fn choice_array_to_string_pulls_text() {
        let content = vec![json!({"text": "part one"}), json!({"text": "part two"})];
        let result = choice_array_to_string(content);
        assert_eq!(result, "part one\npart two");
    }

    #[test]
    fn apply_success_reply_updates_state_and_routes() {
        let reply = AgentReply::Success {
            payload: "{\"result\":42}".into(),
            state_add: Some(vec![StateKeyValue {
                key: "answer".into(),
                value: "42".into(),
                value_type: ValueType::Integer,
            }]),
            state_update: None,
            state_delete: None,
            connections: vec!["node_b".into()],
        };

        let mut ctx = TestContext::new();
        let out = apply_agent_reply(reply, &sample_message(), &mut ctx).unwrap();

        assert_eq!(ctx.state.get("answer"), Some(&StateValue::Integer(42)));
        assert!(matches!(out.routing(), crate::node::Routing::ToNode(name) if name == "node_b"));
    }

    #[test]
    fn need_more_info_routes_to_origin() {
        let reply = AgentReply::NeedMoreInfo {
            payload: FollowUpPayload {
                text: "Clarify".into(),
            },
        };

        let mut ctx = TestContext::new().with_connections(vec!["node".into()]);
        let out = apply_agent_reply(reply, &sample_message(), &mut ctx).unwrap();

        assert!(out.routing().is_reply_to_origin());
        assert_eq!(out.message().payload(), json!({"text": "Clarify"}));
    }
}
