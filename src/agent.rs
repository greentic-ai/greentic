use crate::node::{NodeContext, NodeType};
use crate::{message::Message, node::NodeError};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use schemars::schema::RootSchema;
use serde_json::json;   // assume you have some Executor‐like trait for agents

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "agent")]
pub struct AgentNode {
    /// Which agent to invoke (e.g. “chatgpt”)
    pub agent: String,
    /// Which task or prompt to send (e.g. “summarize”)
    pub task: String,
}

impl AgentNode {
    /// Create a new `AgentNode`
    pub fn new<A: Into<String>, T: Into<String>>(agent: A, task: T) -> Self {
        AgentNode {
            agent: agent.into(),
            task: task.into(),
        }
    }
}

#[typetag::serde]
impl NodeType for AgentNode {
    fn type_name(&self) -> String {
        // this is just for display; you could also return task or combine both
        self.agent.clone()
    }

    fn schema(&self) -> RootSchema {
        // delegates to schemars
        schema_for!(AgentNode)
    }


    fn process(&self, input: Message, _context: &mut NodeContext) -> Result<Message, NodeError> {
        // TODO
        let payload = json!({});
        Ok(Message::new(&input.id(), payload, input.session_id()))
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}
