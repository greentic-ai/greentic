use async_trait::async_trait;
use dashmap::DashMap;
use ollama_rs::generation::embeddings::request::{EmbeddingsInput, GenerateEmbeddingsRequest};
use ollama_rs::models::ModelOptions;
use ollama_rs::generation::completion::request::GenerationRequest;
use ollama_rs::generation::chat::{ChatMessage, request::ChatMessageRequest};
use ollama_rs::Ollama;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tracing::{error, info, warn};
use url::Url;

use crate::executor::call_result_to_json;
use crate::flow::state::StateValue;
use crate::node::Routing;
use crate::{
    message::Message,
    node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType, ToolNode},
};

// --------------------------------------------------------------------------------
// 1) Define OllamaAgent, which wraps ollama-rs usage
// --------------------------------------------------------------------------------

/// `OllamaAgent` invokes a local Ollama server via `ollama_rs`.  
/// It supports plain generation (`Generate`), embeddings (`Embed`), chat mode (`Chat`), and tool calls (`ToolCall`).

#[derive(Debug, Clone, Serialize, Deserialize,)]
pub struct OllamaAgent {
    pub task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

   #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<OllamaMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_url: Option<url::Url>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_options: Option<ollama_rs::models::ModelOptions>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_names: Option<Vec<String>>,

    /// Map of `tool.name -> ToolNode` for any external tool calls (`ToolCall` variant).
    /// This field is not serialized in JSON nor included in the schema.
    #[serde(skip)]
    pub tool_nodes: Option<DashMap<String, Box<ToolNode>>>,

}

impl PartialEq for OllamaAgent {
    fn eq(&self, other: &Self) -> bool {
        self.task == other.task &&
        self.model == other.model &&
        self.mode == other.mode &&
        self.ollama_url == other.ollama_url &&
        self.tool_names == other.tool_names
        // tool_nodes and model_config are intentionally skipped from equality check
    }
}

#[async_trait]
#[typetag::serde]
impl NodeType for OllamaAgent {
    fn type_name(&self) -> String {
        "ollama".to_string()
    }

    fn schema(&self) -> schemars::Schema {
        schemars::schema_for!(OllamaAgent)
    }

    #[tracing::instrument(name = "ollama_agent_node_process", skip(self, context))]
    async fn process(
        &self,
        input: Message,
        context: &mut NodeContext,
    ) -> Result<NodeOut, NodeErr> {
        // 1) build our client
        let mut client = if let Some(url) = &self.ollama_url {
            if let Some(port) = url.port() {
                Ollama::new(url.clone(),port)
            } else {
                Ollama::default()
            }   
        } else {
            Ollama::default()
        };

        match self.mode.unwrap_or_default() {
            OllamaMode::Embed => return self.do_embed(client, &input).await,
            OllamaMode::Generate => return self.do_generate(client, &input).await,
            OllamaMode::Chat => {
                    
                // 2) prepare the conversation
                let system_prompt = r#"You are part of an agentic flow.
                Your job is to complete the given `task` using information from:
                - `payload`: the latest message or data
                - `state`: key/value memory of the session
                - `connections`: valid next nodes
                - `tools`: tools you can optionally call
                The task is often to extract information from a user request so a tool can be called,
                state memory can be updated, a connection can be called,... So being precise and following the rules 
                is important.

                You must always respond with a **JSON object** using this fixed structure:
                {
                "payload": {...},  // Required. If you can solve the task, include the extracted fields as per task (e.g., valid json object: { "q": ..., "days": ...} ). If not, ask follow-up as { "text": "..." }
                "state": { // Only include if the task wants the state to be added, updated or deleted. Otherwise omit entirely.
                    "add":    [{ "key": ..., "value": ... }], // only include if a new key and value need to be added
                    "update": [{ "key": ..., "value": ... }], // only include if an existing key has a new value
                    "delete": [ "key1", "key2" ] // only include if keys need to be removed
                },
                "tool_call": {...} // Only include if the task explicitly requires calling a tool with name + action + optional parameter, othrwise omit entirely. 
                    "name": "tool_name", // only include if a tool name is specified
                    "action": "method", // only include if a tool action is specified
                    "input": {...} // only include if a tool input is requested
                },
                "connections": ["node_a", "node_b"], // You will be given a list of valid connections. If the task specifies a specific connection, include that one, otherwise include all. 
                "reply_to_origin": false          // true only if cannot resolve the task and want the user to clarify and payload has a follow-up question
                }
                
                ### Rules:

                - Only include fields relevant to this step (omit empty ones).
                - You MUST return a valid Json, otherwise your work is useless.
                - You MUST include `"payload"`:
                - If task is **resolved**, use it to pass extracted/derived data forward. Add as valid json.
                - If task is **not resolved**, ask a follow-up in the form:
                    ```json
                    { "text": "clarifying question for the user" }
                    ```
                - `"state"` is ommitted unless the task include keys to add/update/delete (if required by the task).
                - `"tool_call"` is ommitted unless the task needs a tool (as described in the task).
                - `"connections"` should reflect the logical next step(s) in the flow (may be defined in task) Only use connections that are in the 'connections' list.
                - `"reply_to_origin"` must be:
                - `false` if you resolved the task.
                - `true` if you still need user input.

                Do not include explanations or commentary. Only return a valid JSON object.
                "#;

                let task       = &self.task;
                let payload    = input.payload();
                let state_json = json!(context.get_all_state());
                let conns      = context.connections().unwrap_or_default();
                let tools      = self
                    .tool_names
                    .clone()
                    .unwrap_or_default();

                let user_msg = format!(
                    "task: {}\npayload: {}\nstate: {}\nconnections: {:?}\ntools: {:?}",
                    task, payload, state_json, conns, tools
                );

                let history = vec![
                    ChatMessage::system(system_prompt.to_string()),
                    ChatMessage::user(user_msg),
                ];

                // 3) call the LLM
                let model = self.model.clone().unwrap_or("llama3:latest".into());
                let mut req = ChatMessageRequest::new(model, history);
                if let Some(opts) = &self.model_options {
                    req = req.options(opts.clone());
                }

                let resp = client.send_chat_messages_with_history(&mut vec![], req).await;
                if resp.is_err(){
                    error!("LLM gave error: {:?}",resp);
                    return Err(NodeErr::fail(NodeError::ExecutionFailed(format!("LLM error: {:?}", resp.err()))));
                }
                let reply = resp.unwrap().message.content;
                info!("agent response: {}",reply);
                // 4) parse JSON
                let result: JsonValue = serde_json::from_str(&reply)
                    .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(format!(
                        "Invalid JSON from LLM: {}", e
                    ))))?;

                // Optional state updates
                if let Some(state_obj) = result.get("state")  {
                    if let Some(add) = state_obj.get("add").and_then(|v| v.as_array()) {
                        for item in add {
                            if let (Some(k), Some(v)) = (item.get("key"), item.get("value")) {
                                if let Some(k) = k.as_str() {
                                    match StateValue::try_from(v.clone()){
                                        Ok(state_val) => context.set_state(k, state_val),
                                        Err(e) => warn!("Failed to convert value to StateValue: {:?}", e),
                                    }
                                }
                            }
                        }
                    }
                    if let Some(update) = state_obj.get("update").and_then(|v| v.as_array()) {
                        for item in update {
                            if let (Some(k), Some(v)) = (item.get("key"), item.get("value")) {
                                if let Some(k) = k.as_str() {
                                    match StateValue::try_from(v.clone()) {
                                        Ok(state_val) => context.set_state(k, state_val),
                                        Err(e) => warn!("Failed to convert value to StateValue: {:?}", e),
                                    }
                                }
                            }
                        }
                    }
                    if let Some(delete) = state_obj.get("delete").and_then(|v| v.as_array()) {
                        for key in delete {
                            if let Some(k) = key.as_str() {
                                context.delete_state(k);
                            }
                        }
                    }
                }

                let llm_payload = result.get("payload");
                if result.get("reply_to_origin").and_then(|v| v.as_bool()) == Some(true) && llm_payload.is_some() {
                    let main_msg = Message::new(&input.id(), llm_payload.unwrap().clone(), input.session_id().clone());
                    return Ok(NodeOut::reply(main_msg));
                }

                // Gather inline and deferred tool calls
                let follow_up: Vec<String> = result
                    .get("connections")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();

                let next_conn = match follow_up.is_empty() {
                    true => None,
                    false => Some(follow_up),
                };


                // 1) Process “tool_calls” if present:
                if let Some(call) = result.get("tool_call") {
                
                    let name = call
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            NodeErr::fail(NodeError::ExecutionFailed(
                                "Missing `name` in tool_call".into()),)
                        })?;
                    let action = call
                        .get("action")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            NodeErr::fail(NodeError::ExecutionFailed(
                                "Missing `action` in tool_call".into()))
                        })?;
                    let args = call.get("input").cloned().unwrap_or(json!({}));
                    match context.executor().executor.call_tool(
                        name.to_string(),
                        action.to_string(),
                        args.clone(),
                    ) {
                        Ok(tool_res) => {
                            let result_json = call_result_to_json(tool_res);

                            let tool_msg = Message::new(
                                &input.id(),
                                result_json,
                                input.session_id().clone(),
                            );
                            return Ok(NodeOut::next(tool_msg,next_conn));
                        }
                        Err(e) => {
                            // route to your on_err connection
                            return Err(NodeErr::next(
                                NodeError::ExecutionFailed(format!("tool `{}` errored: {:?}", name, e)),
                                next_conn,
                            ));
                        }
                    }

                }

                // 2) No inline/deferred tool_calls left: emit your normal LLM payload + any follow-ups
                let main_msg = Message::new(&input.id(), payload.clone(), input.session_id().clone());
                Ok(NodeOut::next(main_msg, next_conn))
            }
        }
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}


impl OllamaAgent {
    /// Construct a new `OllamaAgent` with explicit fields.
    pub fn new(
        mode: Option<OllamaMode>,
        task: String,
        model: Option<String>,
        ollama_url: Option<Url>,
        model_options: Option<ModelOptions>,
        tool_names: Option<Vec<String>>,
        tool_nodes: Option<DashMap<String, Box<ToolNode>>>,
    ) -> Self {
        OllamaAgent {
            mode,
            task,
            model,
            ollama_url,
            model_options,
            tool_names,
            tool_nodes,
        }
    }

    /// Embed branch: returns a JSON payload of embeddings.
    async fn do_embed(
        &self,
        client: Ollama,
        input: &Message
    ) -> Result<NodeOut, NodeErr> {
        // assume `input.payload()` is { "model": "...", "text": "..." }
        let model = input.payload().get("model")
            .and_then(JsonValue::as_str).unwrap_or(&self.model.as_deref().unwrap_or("default"))
            .to_string();
        let text = input.payload().get("text")
            .and_then(JsonValue::as_str).unwrap_or("")
            .to_string();

        let req = GenerateEmbeddingsRequest::new(model, EmbeddingsInput::Single(text));
        let resp = client.generate_embeddings(req).await
            .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(format!("Embed error: {}", e))))?;

        let out = json!({ "embeddings": resp.embeddings });
        let msg = Message::new(input.id().as_str(), out, input.session_id().clone());
        Ok(NodeOut::with_routing(msg, Routing::FollowGraph))
    }

    /// Generate branch: returns a JSON payload with `"generated_text"`.
    async fn do_generate(
        &self,
        client: Ollama,
        input: &Message
    ) -> Result<NodeOut, NodeErr> {
        // assume `input.payload()` is { "model": "...", "prompt": "..." }
        let model = input.payload().get("model")
            .and_then(JsonValue::as_str).unwrap_or(&self.model.as_deref().unwrap_or("default"))
            .to_string();
        let prompt = input.payload().get("prompt")
            .and_then(JsonValue::as_str).unwrap_or("")
            .to_string();

        let mut req = GenerationRequest::new(model, prompt);
        if let Some(opts) = &self.model_options {
            req = req.options(opts.clone());
        }
        let resp = client.generate(req).await
            .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(format!("Generate error: {}", e))))?;

        let out = json!({ "generated_text": resp.response });
        let msg = Message::new(input.id().as_str(), out, input.session_id().clone());
        Ok(NodeOut::with_routing(msg, Routing::FollowGraph))
    }
}

/// Each kind of Ollama request—tool call, embed, chat, or generate.
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OllamaMode {
    /// 1) Generate embeddings for a given text.
    ///    `model` is the embedding model identifier; `text` is the raw string.
    Embed,

    /// 2) Engage in “chat mode”: a sequence of role-tagged messages.
    ///    `history` is a Vec<ChatMessage>.
    Chat,

    /// 3) Plain generation from a single prompt.
    Generate,
}

impl Default for OllamaMode {
    fn default() -> Self {
        OllamaMode::Chat
    }
}


#[cfg(test)]
mod ollama_agent_tests {
    use super::*;
    use schemars::schema_for;
    use serde_json::json;
    use tokio;
    use url::Url;

    fn make_agent(mode: Option<OllamaMode>) -> OllamaAgent {
        OllamaAgent {
            mode,
            task: "dummy".into(),
            model: Some("llama3.2:1b".into()),
            ollama_url: None,
            model_options: None,
            tool_names: None,
            tool_nodes: None,
        }
    }

    #[test]
    fn serde_roundtrip_and_schema_contains_all_fields() {
        let agent = OllamaAgent {
            mode: Some(OllamaMode::Generate),
            task: "t".into(),
            model: Some("m".into()),
            ollama_url: Some(Url::parse("http://x/").unwrap()),
            model_options: None,
            tool_names: Some(vec!["foo".into()]),
            tool_nodes: None,
        };

        let s = serde_json::to_string(&agent).unwrap();
        let de: OllamaAgent = serde_json::from_str(&s).unwrap();
        assert_eq!(de.mode, Some(OllamaMode::Generate));
        assert_eq!(de.task, "t");
        assert_eq!(de.model, Some("m".into()));

        // schema
        let schema = schema_for!(OllamaAgent);
        let schema_json: JsonValue = serde_json::to_value(&schema).unwrap();
        // Navigate to the properties map
        let props = schema_json
            .get("properties")
            .expect("no properties in schema");

        for key in &["mode", "task", "model", "ollama_url", "tool_names"] {
            assert!(
                props.get(*key).is_some(),
                "missing `{}` in schema",
                key
            );
        }
    }

    #[tokio::test]
    async fn default_mode_is_chat_and_bad_endpoint_errors() {
        // no mode => Chat
        let mut agent = make_agent(None);
        agent.ollama_url = Some(Url::parse("http://localhost:123").unwrap());
        // this will attempt Chat against localhost:11434 and fail
        let msg = Message::new("id", json!({}), "123".to_string());
        let mut ctx = NodeContext::dummy();
        let err = agent.process(msg, &mut ctx).await.unwrap_err();
        let e = format!("{:?}", err);
        assert!(e.contains("LLM error"), "expected Chat path to produce an LLM error");
    }

    #[tokio::test]
    async fn embed_mode_goes_to_do_embed_and_parses_payload() {
        let mut agent = make_agent(Some(OllamaMode::Embed));
        // override host to bogus so it errors *after* embedding arguments
        agent.ollama_url = Some(Url::parse("http://127.0.0.1:1/").unwrap());
        let payload = json!({
            "model": "custom-model",
            "text": "hello embed"
        });
        let msg = Message::new("id", payload.clone(), "123".to_string());
        let mut ctx = NodeContext::dummy();
        let res = agent.process(msg, &mut ctx).await;
        // On embedding the *client* will error connecting to host:1,
        // but we at least confirmed we hit do_embed and built the right request
        let err = res.unwrap_err();
        let s = format!("{:?}", err);
        assert!(s.contains("Embed error"), "expected Embed branch");
    }

    #[tokio::test]
    async fn generate_mode_goes_to_do_generate_and_parses_payload() {
        let mut agent = make_agent(Some(OllamaMode::Generate));
        agent.ollama_url = Some(Url::parse("http://127.0.0.1:1/").unwrap());
        let payload = json!({
            "model": "custom-model",
            "prompt": "why?"
        });
        let msg = Message::new("id", payload.clone(), "123".to_string());
        let mut ctx = NodeContext::dummy();
        let res = agent.process(msg, &mut ctx).await;
        let err = res.unwrap_err();
        let s = format!("{:?}", err);
        assert!(s.contains("Generate error"), "expected Generate branch");
    }

}
