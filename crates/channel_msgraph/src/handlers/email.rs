use std::sync::Arc;

use crate::handlers::{interpolate_key, SubscriptionField, SubscriptionHandler};
use crate::{graph_client::GraphState, handlers::SubscriptionSpec};
use channel_plugin::message::{ChannelMessage, MessageContent};
use chrono::{Duration, Utc};
use dashmap::DashMap;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;
use graph_rs_sdk::http::Body;
use base64::{engine::general_purpose, Engine as _};

pub struct EmailHandler;

impl EmailHandler{
    pub async fn send_email(
        &self,
        msg: &ChannelMessage,
        _config: Arc<DashMap<String, String>>,
        graph_state: Arc<RwLock<GraphState>>,
    ) -> anyhow::Result<()> {
   
        // 📨 From: required for Graph /users/{id}/sendMail
        let from_user = msg.from.id.clone();

        let to_recipients: Vec<serde_json::Value> = msg
            .to
            .iter()
            .map(|p| {
                json!({
                    "emailAddress": {
                        "address": p.id.clone()
                    }
                })
            })
            .collect();

        if to_recipients.is_empty() {
            return Err(anyhow::anyhow!("No valid 'to' recipients with email IDs"));
        }

        let mut text_parts = msg.content.iter().filter_map(|c| {
        if let MessageContent::Text { text } = c {
                Some(text.as_str())
            } else {
                None
            }
        });

        let subject = text_parts.next().unwrap_or("No subject");
        let body = text_parts.next().unwrap_or("No body");

        let content_type = if body.to_lowercase().contains("<html") || body.to_lowercase().contains("<body") {
            "HTML"
        } else {
            "Text"
        };

        let attachments = generate_attachments(msg).await;

        let email_json = serde_json::json!({
            "message": {
                "subject": subject,
                "body": {
                    "contentType": content_type,
                    "content": body
                },
                "toRecipients": to_recipients,
                "attachments": attachments
            },
            "saveToSentItems": "true"
        });

        let body = Body::from(email_json.to_string());

        let mut graph = graph_state.read().await.client.clone();

        let response = graph
            .v1()
            .users()
            .id(&from_user)
            .send_mail(body) // pass it directly
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Failed to send mail: {} - {}", status, text);
        }

        info!("📧 Sent email to {:?}", msg.to.iter().map(|p| &p.id).collect::<Vec<_>>());
        Ok(())
    }
}

pub async fn generate_attachments(msg: &ChannelMessage) -> Vec<serde_json::Value> {
    let mut attachments = vec![];
    let client = Client::new();

    for content in &msg.content {
        let maybe_file = match content {
            MessageContent::File { file } => Some(file),
            MessageContent::Media { media } => Some(&media.file),
            _ => None,
        };

        if let Some(file) = maybe_file {
            match client.get(&file.url).send().await {
                Ok(resp) => match resp.bytes().await {
                    Ok(bytes) => {
                        let encoded = general_purpose::STANDARD.encode(&bytes);
                        attachments.push(json!({
                            "@odata.type": "#microsoft.graph.fileAttachment",
                            "name": file.file_name,
                            "contentType": file.mime_type,
                            "contentBytes": encoded,
                        }));
                    }
                    Err(err) => {
                        tracing::warn!("⚠️ Failed to read file content from {}: {:?}", file.url, err);
                    }
                },
                Err(err) => {
                    tracing::warn!("⚠️ Failed to download file from {}: {:?}", file.url, err);
                }
            }
        }
    }

    attachments
}

#[async_trait::async_trait]
impl SubscriptionHandler for EmailHandler {
    fn spec(&self) -> SubscriptionSpec {
        email_spec()
    }

    async fn add(
        &self,
        config: Arc<DashMap<String, String>>,
        graph_state: Arc<RwLock<GraphState>>,
    ) -> anyhow::Result<()> {
        email_add(config, graph_state).await
    }

    async fn delete(
        &self,
        config: Arc<DashMap<String, String>>,
        graph_state: Arc<RwLock<GraphState>>,
    ) -> anyhow::Result<()> {
        email_delete(config, graph_state).await
    }

    async fn send_message(
        &self,
        msg: &ChannelMessage,
        config: Arc<DashMap<String, String>>,
        graph_state: Arc<RwLock<GraphState>>,
    ) -> Option<anyhow::Result<()>> {
        Some(self.send_email(msg, config, graph_state).await)
    }
}

pub async fn email_add(
    config: Arc<DashMap<String, String>>,
    graph_state: Arc<RwLock<GraphState>>,
) -> anyhow::Result<()> {
    let state = graph_state.read().await;
    let domain = state.config.domain.clone();
    let mut graph = state.client.clone();
    drop(state);

    let mut payload_map = serde_json::Map::new();
    for entry in config.iter() {
        payload_map.insert(entry.key().clone(), Value::String(entry.value().clone()));
    }
    let payload = Value::Object(payload_map);

    let spec = email_spec();

    for field in &spec.fields {
        if field.required && !payload.get(field.key).and_then(|v| v.as_str()).is_some() {
            return Err(anyhow::anyhow!(format!("Missing required field: {}", field.key)));
        }
    }

    let key = interpolate_key(&spec.key_template, &payload)
        .ok_or_else(|| anyhow::anyhow!("Invalid key interpolation"))?;

    info!("Registering email subscription with key: {}", key);

    let user_id = payload
        .get("email_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("me");

    let url = format!("https://{}/notification", domain);
    let expiration = Utc::now() + Duration::minutes(4230);
    let resource = format!("/users/{}/messages", user_id);

    let subscription = json!({
        "changeType": "created,updated",
        "notificationUrl": url,
        "resource": resource,
        "expirationDateTime": expiration.to_rfc3339(),
        "clientState": "email_state"
    });

    graph
        .v1()
        .subscriptions()
        .create_subscription(&subscription)
        .send()
        .await?;

    Ok(())
}

pub async fn email_delete(
    config: Arc<DashMap<String, String>>,
    graph_state: Arc<RwLock<GraphState>>,
) -> anyhow::Result<()> {
    let state = graph_state.read().await;
    let mut graph = state.client.clone();
    drop(state);

    let mut payload_map = serde_json::Map::new();
    for entry in config.iter() {
        payload_map.insert(entry.key().clone(), Value::String(entry.value().clone()));
    }
    let payload = Value::Object(payload_map);

    let user_id = payload
        .get("email_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("me");

    let resource = format!("/users/{}/messages", user_id);
    let subs: Value = graph.v1().subscriptions().list_subscription().send().await?.json().await?;
    if let Some(items) = subs.get("value").and_then(|v| v.as_array()) {
        for item in items {
            if item.get("resource").and_then(|v| v.as_str()) == Some(&resource) {
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    graph.v1().subscription(id).delete_subscription().send().await?;
                    info!("Deleted subscription with id: {}", id);
                }
            }
        }
    }

    Ok(())
}

pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let mut states = vec![];

    for key in config.iter().map(|kv| kv.key().clone()) {
        if let Some(stripped) = key.strip_prefix("msgraph:") {
            let parts: Vec<&str> = stripped.split(':').collect();
            if parts.len() == 5 && parts[2] == "email" {
                let user_id = parts[4];
                states.push((format!("/users/{}/messages", user_id), "email_state".to_string()));
            }
        }
    }

    states
}

pub fn email_spec() -> SubscriptionSpec {
    SubscriptionSpec {
        subscription_type: "email-user",
        description: "Monitor email messages for a user.",
        key_template: "msgraph:{tenant_id}:{domain}:email:{email_user_id}",
        fields: vec![
            SubscriptionField {
                key: "tenant_id",
                required: true,
                description: "Azure tenant ID",
            },
            SubscriptionField {
                key: "domain",
                required: true,
                description: "Domain (e.g., contoso.com)",
            },
            SubscriptionField {
                key: "email_user_id",
                required: true,
                description: "User ID for the mailbox",
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use crate::config::MsGraphConfig;

    use super::*;
    use channel_plugin::message::{FileMetadata, Participant};
    use graph_rs_sdk::Graph;
    use mockito::Server;
    use serde_json::json;
    use dashmap::DashMap;

    pub fn mock_graph_state() -> GraphState {
        let config = MsGraphConfig {
            tenant_id: "test-tenant".into(),
            domain: "example.com".into(),
            client_id: "client-id".into(),
            client_secret: "client-secret".into(),
        };

        let client = Graph::new(""); // No base URL needed for test

        GraphState { config, client }
    }

    #[test]
    fn test_interpolate_key_success() {
        let spec = email_spec();
        let payload = json!({
            "tenant_id": "tenant123",
            "domain": "example.com",
            "email_user_id": "user456"
        });
        let key = interpolate_key(&spec.key_template, &payload);
        assert_eq!(
            key.unwrap(),
            "msgraph:tenant123:example.com:email:user456"
        );
    }

    #[test]
    fn test_interpolate_key_missing_value() {
        let spec = email_spec();
        let payload = json!({
            "tenant_id": "tenant123",
            "email_user_id": "user456"
        });
        let key = interpolate_key(&spec.key_template, &payload);
        assert!(key.is_none());
    }

    #[test]
    fn test_email_spec_fields_required() {
        let spec = email_spec();
        assert_eq!(spec.subscription_type, "email-user");
        assert_eq!(spec.fields.len(), 3);
        for field in &spec.fields {
            assert!(field.required);
        }
    }

    #[test]
    fn test_get_client_states_extraction() {
        let config = DashMap::new();
        config.insert(
            "msgraph:tenant123:example.com:email:user456".to_string(),
            "value".to_string(),
        );
        config.insert(
            "msgraph:tenant123:example.com:calendar:user456".to_string(),
            "value".to_string(),
        ); // Should not be included

        let states = get_client_states(&config);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].0, "/users/user456/messages");
        assert_eq!(states[0].1, "email_state");
    }

    #[tokio::test]
    async fn test_send_email_html_detection() {
        let handler = EmailHandler;
        let msg = ChannelMessage {
            from: Participant { id: "sender@example.com".to_string(), ..Default::default() },
            to: vec![Participant { id: "recipient@example.com".to_string(), ..Default::default() }],
            content: vec![
                MessageContent::Text { text: "Subject Test".to_string() },
                MessageContent::Text { text: "<html><body>Body</body></html>".to_string() }
            ],
            ..Default::default()
        };
        let config = Arc::new(DashMap::new());
        let graph_state = Arc::new(RwLock::new(mock_graph_state()));
        let result = handler.send_email(&msg, config, graph_state).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_generate_attachments_with_mockito_server() {
        let mut server = Server::new_async().await;

        // Set up mock GET /file endpoint
        let _m = server
            .mock("GET", "/file")
            .with_status(200)
            .with_body("mocked content")
            .create_async()
            .await;

        let url = format!("{}/file", server.url()); // use server.url()

        let msg = ChannelMessage {
            content: vec![MessageContent::File {
                file: FileMetadata {
                    file_name: "test.txt".into(),
                    mime_type: "text/plain".into(),
                    url,
                    size_bytes: Some(13),
                },
            }],
            ..Default::default()
        };

        let attachments = super::generate_attachments(&msg).await;
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0]["name"], "test.txt");
        assert_eq!(attachments[0]["contentType"], "text/plain");
    }

    #[tokio::test]
    async fn test_send_email_missing_recipient() {
        let handler = EmailHandler;
        let msg = ChannelMessage {
            from: Participant { id: "sender@example.com".to_string(), ..Default::default() },
            to: vec![],
            content: vec![
                MessageContent::Text { text: "Subject".to_string() },
                MessageContent::Text { text: "Body".to_string() },
            ],
            ..Default::default()
        };
        let config = Arc::new(DashMap::new());
        let graph_state = Arc::new(RwLock::new(mock_graph_state()));
        let result = handler.send_email(&msg, config, graph_state).await;
        assert!(result.is_err());
    }
}
