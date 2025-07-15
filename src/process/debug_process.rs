use ::serde::{Deserialize, Serialize};
use async_trait::async_trait;
use schemars::{JsonSchema, Schema, schema_for};
use tracing::info;

use crate::{
    message::Message,
    node::{NodeContext, NodeErr, NodeOut, NodeType, Routing},
};

/// A simple debug node for testing that just echoes its input.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename = "debug")]
pub struct DebugProcessNode {
    #[serde(
        default = "DebugProcessNode::default_print",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub print: bool,
}

impl DebugProcessNode {
    fn default_print() -> bool {
        false
    }
}

impl Default for DebugProcessNode {
    fn default() -> Self {
        DebugProcessNode { print: false }
    }
}

#[async_trait]
#[typetag::serde]
impl NodeType for DebugProcessNode {
    fn type_name(&self) -> String {
        "debug".to_string()
    }

    fn schema(&self) -> Schema {
        schema_for!(DebugProcessNode)
    }

    #[tracing::instrument(name = "debug_node_process", skip(self, context))]
    async fn process(&self, input: Message, context: &mut NodeContext) -> Result<NodeOut, NodeErr> {
        if self.print {
            println!(
                "**** DEBUG ****: Got message {:?}, context {:?}",
                input,
                context.get_all_state()
            );
        }

        info!(
            "**** DEBUGGER ****: Got message {:?}, context {:?}",
            input,
            context.get_all_state()
        );
        Ok(NodeOut::with_routing(input, Routing::FollowGraph))
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod debug_tests {
    use super::*;
    use dashmap::DashMap;
    use schemars::schema_for;
    use serde_json::json;

    // bring in your dummy constructors:
    use crate::channel::manager::ChannelManager;
    use crate::executor::Executor;
    use crate::flow::manager::NodeKind;
    use crate::flow::state::InMemoryState;
    use crate::node::Routing;
    use crate::process::manager::{BuiltInProcess, ProcessManager};
    use crate::secret::{EmptySecretsManager, SecretsManager};

    #[tokio::test]
    async fn serde_roundtrip_json_and_yaml() {
        let node = DebugProcessNode { print: true };

        // JSON round‐trip
        let j = serde_json::to_string(&node).expect("serialize to JSON");
        let node2: DebugProcessNode = serde_json::from_str(&j).expect("deserialize JSON");
        assert_eq!(node, node2);

        // YAML round‐trip
        let y = serde_yaml_bw::to_string(&node).expect("serialize to YAML");
        let node3: DebugProcessNode = serde_yaml_bw::from_str(&y).expect("deserialize YAML");
        assert_eq!(node, node3);

        // Schema contains "debug"
        let schema = schema_for!(DebugProcessNode);
        let obj = schema.as_object().expect("Expected schema to be an object");
        let title = obj
            .get("title")
            .and_then(|v| v.as_str())
            .expect("Expected 'title' to be a string");

        assert_eq!(title, "debug");
    }

    #[test]
    fn type_name_is_debug() {
        let node = DebugProcessNode { print: true };
        assert_eq!(node.type_name(), "debug");
    }

    #[tokio::test]
    async fn process_echoes_message_and_no_connections() {
        // build a dummy Message
        let original = Message::new(
            "msg1",
            json!({ "foo": "bar", "n": 42 }),
            "sess-123".to_string(),
        );

        // build a minimal NodeContext
        let mut ctx = NodeContext::new(
            "123".to_string(),
            InMemoryState::new(),
            DashMap::new(),
            Executor::dummy(),
            ChannelManager::dummy(),
            ProcessManager::dummy(),
            SecretsManager(EmptySecretsManager::new()),
            None,
        );

        // call process
        let node = DebugProcessNode { print: true };
        let out = node
            .process(original.clone(), &mut ctx)
            .await
            .expect("process should succeed");

        // out should be NodeOut::all(original) ⇒ one message, no next‐connections
        let msgs = out.message(); // Vec<Message>
        let routing = out.routing(); // Option<&[String]>

        assert_eq!(msgs, original);
        assert_eq!(routing, &Routing::FollowGraph);
    }

    #[test]
    fn debug_node_yaml_example() {
        // wrap your DebugProcessNode in a NodeKind
        let kind = NodeKind::Process {
            process: BuiltInProcess::Debug(DebugProcessNode { print: true }),
        };
        // serialize to YAML
        let yaml = serde_yaml_bw::to_string(&kind).unwrap();
        // print it so you can see exactly the shape
        println!("\n>> debug‐node YAML:\n{}", yaml);
    }
}
