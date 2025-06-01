use crate::{node::{NodeContext, NodeErr, NodeOut}};
use crate::message::Message;
// assume you have some Executor‐like trait for agents



pub type AgentResult = Result<NodeOut, NodeErr>;
/// Each concrete agent backend must register under a unique “type name” here.
/// The `typetag::serde` macro machinery will emit a hidden “type” field in JSON,
/// e.g. `{ "type": "chatgpt", "api_key": "XXX" }`, and choose the correct struct to deserialize.
pub trait AgentNode: Send + Sync {
    /// Returns the unique key under which this executor is registered,
    /// e.g. "chatgpt", "ollama", "deepseek".
    fn agent_name(&self) -> &'static str;

    /// Given a `task` string (e.g. "summarize", "classify", "translate"),
    /// an input `Message` payload, and the global `NodeContext`, produce a new
    /// `Message` as the output. Return an `Error` on failure.
    fn execute(
        &self,
        task: &str,
        input: Message,
        ctx: &mut NodeContext,
    ) -> AgentResult;

    /// Allows cloning a boxed executor. Each implementation must provide this.
    fn clone_box(&self) -> Box<dyn AgentNode>;
}


// Required to let trait‐objects be cloned via `.clone_box()`
impl Clone for Box<dyn AgentNode> {
    fn clone(&self) -> Box<dyn AgentNode> {
        self.clone_box()
    }
}

