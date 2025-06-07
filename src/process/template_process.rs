use async_trait::async_trait;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use ::serde::{Deserialize, Serialize};
use handlebars::{Handlebars, JsonValue};
use serde_json::json;

use crate::{message::Message, node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType}};

/// A Handlebars template process node.
///
/// This node renders a string template using Handlebars syntax with access to:
/// - `msg`: the entire incoming message (e.g., `{{msg.id}}`, `{{msg.session_id}}`, `{{msg.payload.text}}`)
/// - `payload`: the message payload, automatically parsed into JSON (e.g., `{{payload.weather.temp}}`)
/// - `state`: the node state context (e.g., `{{state.age}}`, `{{state.user.name}}`)
///
/// ### Basic Examples
///
/// #### Static template
/// ```handlebars
/// Hello world!
/// ```
///
/// #### Message metadata
/// ```handlebars
/// Hello {{msg.id}}, your session is {{msg.session_id}}.
/// ```
///
/// #### Payload values
/// ```handlebars
/// Weather today is {{payload.weather.temp}}°C.
/// ```
///
/// #### State values
/// ```handlebars
/// User: {{state.user.name}}, Age: {{state.age}}.
/// ```
///
/// ### Conditional Logic
///
/// #### Basic `if` / `else`
/// ```handlebars
/// {{#if state.is_admin}}
/// Welcome, administrator!
/// {{else}}
/// Welcome, user!
/// {{/if}}
/// ```
///
/// #### Check existence
/// ```handlebars
/// {{#if payload.alert}}
/// ⚠️ Alert: {{payload.alert.message}}
/// {{/if}}
/// ```
///
/// ### Iteration
///
/// #### Loop over an array in the payload
/// ```handlebars
/// Your items:
/// {{#each payload.cart}}
/// - {{this.name}} ({{this.qty}} pcs)
/// {{/each}}
/// ```
///
/// ### Nested values
/// ```handlebars
/// {{state.profile.address.city}}, {{state.profile.address.country}}
/// ```
///
/// ### Escaping and raw output
/// - To escape output: `{{msg.id}}` (HTML-escaped)
/// - To output raw (unescaped): `{{{msg.payload.html}}}`
///
/// ### Notes
/// - Missing fields render as empty strings.
/// - Complex conditionals (e.g., `==`, `>`, `&&`) require custom helpers (not supported by default).
///
/// For advanced usage and custom helpers, refer to the [Handlebars Rust docs](https://docs.rs/handlebars).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename = "template")]
pub struct TemplateProcessNode {
    pub template: String,
}

#[async_trait]
#[typetag::serde]
impl NodeType for TemplateProcessNode {
    fn type_name(&self) -> String {
        "template".to_string()
    }

    fn schema(&self) -> RootSchema {
        schema_for!(TemplateProcessNode)
    }

    #[tracing::instrument(name = "template_node_process", skip(self, context))]
    async fn process(
        &self,
        input: Message,
        context: &mut NodeContext,
    ) -> Result<NodeOut, NodeErr> {
        let hbs = Handlebars::new();

        // Combine inputs into one context map
        let mut data = serde_json::Map::new();
        data.insert("msg".to_string(), serde_json::to_value(&input).unwrap_or(json!({})));
        data.insert(
            "state".to_string(),
            serde_json::to_value(context.get_all_state()).unwrap_or(json!({})),
        );

        // Parse payload if it's a stringified JSON
        let payload_value = match &input.payload() {
            serde_json::Value::String(s) => {
                serde_json::from_str::<JsonValue>(s).unwrap_or(json!({}))
            }
            other => other.clone(),
        };
        data.insert("payload".to_string(), payload_value);

        // Render the template
        let rendered = hbs.render_template(&self.template, &data)
            .map_err(|e| NodeErr::all(NodeError::InvalidInput(format!("Template render error: {}", e))))?;

        let msg = Message::new(&input.id(), json!({"text": rendered}), input.session_id());
        Ok(NodeOut::all(msg))
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeContext};
    use crate::message::Message;
    use crate::state::StateValue;
    use serde_json::json;




    fn dummy_context_with_state() -> NodeContext {
        let mut ctx = NodeContext::dummy();
        ctx.set_state("age", StateValue::try_from(json!(30)).unwrap());
        ctx.set_state("user", StateValue::try_from(json!({"name": "Alice"})).unwrap());
        ctx
    }

    #[tokio::test]
    async fn test_basic_template_rendering() {
        let node = TemplateProcessNode {
            template: "Hello world!".to_string(),
        };

        let msg = Message::new("test_id", json!({}), None);
        let mut ctx = NodeContext::dummy();

        let output = node.process(msg.clone(), &mut ctx).await.unwrap();
        let payload = output.message().payload();
        let text = payload["text"].as_str().unwrap();
        assert_eq!(text, "Hello world!");
    }

    #[tokio::test]
    async fn test_template_with_msg_state_payload() {
        let node = TemplateProcessNode {
            template: "Hi {{msg.id}}, you are {{state.age}} and it's {{payload.weather.temp}}°C.".to_string(),
        };

        let msg = Message::new("abc123", json!({"weather": {"temp": 21}}), Some("sess42".to_string()));
        let mut ctx = dummy_context_with_state();

        let output = node.process(msg.clone(), &mut ctx).await.unwrap();
        let payload = output.message().payload();
        let text = payload["text"].as_str().unwrap();
        assert_eq!(text, "Hi abc123, you are 30.0 and it's 21°C.");
    }

    #[tokio::test]
    async fn test_stringified_json_payload() {
        let node = TemplateProcessNode {
            template: "Temperature: {{payload.weather.temp}}".to_string(),
        };

        let json_string = r#"{"weather": {"temp": 17}}"#;
        let msg = Message::new("abc123", json_string.into(), None);
        let mut ctx = NodeContext::dummy();

        let output = node.process(msg.clone(), &mut ctx).await.unwrap();
        let payload = output.message().payload();
        let text = payload["text"].as_str().unwrap();
        assert_eq!(text, "Temperature: 17");
    }

    #[tokio::test]
    async fn test_missing_template_fields_gracefully() {
        let node = TemplateProcessNode {
            template: "Hello {{state.name}}, temp: {{payload.temp}}".to_string(),
        };

        let msg = Message::new("id", json!({}), None);
        let mut ctx = NodeContext::dummy();

        let output = node.process(msg.clone(), &mut ctx).await.unwrap();
        let payload = output.message().payload();
        let text = payload["text"].as_str().unwrap();
        assert_eq!(text, "Hello , temp: ");
    }
}
