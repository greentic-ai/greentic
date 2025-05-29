use crate::node::{NodeContext, NodeType};
use crate::{message::Message, node::NodeError};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use schemars::schema::RootSchema;
use serde_json::{json, Value};   // assume you have some Executor‐like trait for agents

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "process")]
pub struct ProcessNode {
    pub name: String,
    /// which arguments are needed for the process
    pub args: Value,

}

impl ProcessNode {
    /// Create a new `AgentNode`
    pub fn new<N: Into<String>,A: Into<Value>>(name: N, args: A, ) -> Self {
        ProcessNode {
            name: name.into(),
            args: args.into(),
        }
    }
}

#[typetag::serde]
impl NodeType for ProcessNode {
    fn type_name(&self) -> String {
        self.name.clone()
    }

    fn schema(&self) -> RootSchema {
        // delegates to schemars
        schema_for!(ProcessNode)
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
