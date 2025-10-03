// remote_plugin_core/src/remote_plugin.rs

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::SystemTime;

// ---- pull these from your existing crate -------------------
use crate::message::{ChannelCapabilities, ChannelMessage, JsonSchemaDescriptor, MessageContent}; // from your message.rs module

// ---- Admin (control-plane) events --------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminEvent {
    /// Create or update a provider subscription / webhook / watch
    UpsertSubscription {
        tenant: String,
        /// logical channel name, e.g. "ms_teams", "ms_email"
        channel: String,
        /// channel-specific params (validated with ChannelCapabilities.channel_data_schema)
        params: Value,
    },

    /// Remove an existing subscription
    DeleteSubscription {
        tenant: String,
        channel: String,
        /// optional subscription id / key inside provider
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sub_id: Option<String>,
    },

    /// Token/credential tracked by the scheduler (see RefreshRegistry)
    UpsertToken {
        tenant: String,
        /// logical token key (e.g. "ms_graph:acme:app")
        key: String,
        /// reference into your SecretsManager (not the secret itself)
        secret_ref: String,
        /// when to refresh/rotate; if None, handler decides
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<SystemTime>,
    },

    CancelToken {
        tenant: String,
        key: String,
    },

    /// Optional: expose capabilities explicitly (useful for smoke tests)
    GetCapabilities,
}

// ---- Refresh outcomes --------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// New expiry; scheduler should (re)register for this time
    Rescheduled(SystemTime),
    /// No longer needed; scheduler should cancel future runs
    Cancelled,
    /// Keep same schedule (no change)
    Unchanged,
}

// ---- Cross-platform scheduler SPI --------------------------

#[async_trait]
pub trait RefreshRegistry: Send + Sync {
    /// Register/extend a refresh invocation for (tenant,key) at `at`.
    /// Idempotent. If `at` is in the past, schedule ASAP.
    async fn upsert(&self, tenant: String, key: String, at: SystemTime) -> anyhow::Result<()>;

    /// Cancel future refreshes for (tenant,key).
    async fn cancel(&self, tenant: &str, key: &str) -> anyhow::Result<()>;
}

// ---- Remote handler API (business logic) -------------------

#[async_trait]
pub trait RemotePluginHandler: Send + Sync + 'static {
    /// Capability document for this channel/connector.
    /// Returned once at startup and cacheable by the adapter.
    async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities>;

    /// Provider -> Greentic: incoming webhook/event to be forwarded to NATS
    async fn receive(&self, msg: ChannelMessage) -> anyhow::Result<()>;

    /// Greentic -> Provider: send an outbound message/action
    async fn send(&self, msg: ChannelMessage) -> anyhow::Result<()>;

    /// Admin/control operations (subscriptions, tokens, etc.)
    async fn admin(&self, evt: AdminEvent) -> anyhow::Result<Value>;

    /// Called by the scheduler when (tenant,key) is due for refresh.
    /// Return `Rescheduled(t)` to push the next alarm, or `Cancelled` to stop.
    async fn refresh(&self, tenant: &str, key: &str) -> anyhow::Result<RefreshOutcome>;
}

pub fn validate_channel_data_with_caps(
    caps: &ChannelCapabilities,
    channel_data: &serde_json::Value,
) -> Result<(), String> {
    let Some(desc) = &caps.channel_data_schema else {
        return Ok(());
    };

    // JsonSchemaDescriptor is your enum (Inline/Ref). For tests we only handle Inline.
    let schema_val = match desc {
        JsonSchemaDescriptor::Inline { schema, .. } => schema.clone(),
        JsonSchemaDescriptor::Ref { .. } => {
            return Err("Ref schema provided but not resolved".to_string());
        }
    };

    // jsonschema = "0.32.1"
    match jsonschema::validate(&schema_val, channel_data) {
        Ok(()) => Ok(()),
        Err(e) => Err(format!("channel_data validation failed: {e}")),
    }
}

// ---- Subject/idempotency helpers (optional) ----------------

/// Build a NATS subject from a message for in/out/admin planes.
/// Customize to your conventions.
pub fn subject_for_in(msg: &ChannelMessage) -> String {
    // tenant optional; session_id or thread_id can help shard
    let tenant = msg
        .metadata
        .get("tenant")
        .and_then(|v| v.as_str())
        .unwrap_or("anon");
    let kind = msg
        .content
        .first()
        .map(|c| match c {
            MessageContent::Text { .. } => "text",
            MessageContent::File { .. } => "file",
            MessageContent::Media { .. } => "media",
            MessageContent::Event { event } => event.event_type.as_str(),
        })
        .unwrap_or("unknown");
    format!("greentic.in.{}.{}.{}", msg.channel, tenant, kind)
}

pub fn subject_for_out(msg: &ChannelMessage) -> String {
    let tenant = msg
        .metadata
        .get("tenant")
        .and_then(|v| v.as_str())
        .unwrap_or("anon");
    let kind = msg
        .content
        .first()
        .map(|c| match c {
            MessageContent::Text { .. } => "text",
            MessageContent::File { .. } => "file",
            MessageContent::Media { .. } => "media",
            MessageContent::Event { event } => event.event_type.as_str(),
        })
        .unwrap_or("unknown");
    format!("greentic.out.{}.{}.{}", msg.channel, tenant, kind)
}

/// If producer didn’t set one, derive an idempotency key deterministically.
pub fn idempotency_key_or_hash(msg: &ChannelMessage) -> String {
    if let Some(v) = msg.metadata.get("idempotency_key").and_then(|v| v.as_str()) {
        return v.to_string();
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(msg.id.as_bytes());
    hasher.update(msg.channel.as_bytes());
    hasher.update(msg.timestamp.as_bytes());
    hasher.update(serde_json::to_vec(&msg.content).unwrap());
    format!("msg:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ChannelMessage, JsonSchemaDescriptor, MessageDirection, Participant};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    // ---- helpers ------------------------------------------------------------

    // Adjust this to build a minimal valid `Participant` from your codebase.
    fn dummy_participant() -> Participant {
        #[allow(clippy::default_trait_access)]
        Participant::default()
    }

    fn msg_text(id: &str, channel: &str, text: &str) -> ChannelMessage {
        ChannelMessage {
            id: id.into(),
            session_id: Some("sess-1".into()),
            direction: MessageDirection::Incoming,
            channel: channel.into(),
            from: dummy_participant(),
            to: vec![],
            timestamp: "2025-08-08T08:00:00Z".into(),
            content: vec![MessageContent::Text { text: text.into() }],
            thread_id: Some("thread-1".into()),
            reply_to_id: None,
            // if you added tenant/channel_data/idempotency_key: they can remain default/null
            ..ChannelMessage::default()
        }
    }

    // ---- AdminEvent serde ---------------------------------------------------

    #[test]
    fn admin_event_roundtrip() {
        let evt = AdminEvent::UpsertSubscription {
            tenant: "acme".into(),
            channel: "ms_teams".into(),
            params: json!({"team_id":"t1","channel_id":"c1"}),
        };
        let s = serde_json::to_string(&evt).unwrap();
        let back: AdminEvent = serde_json::from_str(&s).unwrap();
        match back {
            AdminEvent::UpsertSubscription {
                tenant,
                channel,
                params,
            } => {
                assert_eq!(tenant, "acme");
                assert_eq!(channel, "ms_teams");
                assert_eq!(params["team_id"], "t1");
            }
            _ => panic!("wrong variant"),
        }
    }

    // ---- Subjects & idempotency --------------------------------------------

    #[test]
    fn subjects_build_as_expected() {
        let mut msg = msg_text("m1", "ms_teams", "Hello");
        // If you treat tenant via metadata, seed it:
        msg.metadata = json!({"tenant":"acme"});
        let s_in = subject_for_in(&msg);
        let s_out = subject_for_out(&msg);
        assert!(s_in.starts_with("greentic.in.ms_teams.acme."));
        assert!(s_out.starts_with("greentic.out.ms_teams.acme."));
    }

    #[test]
    fn idempotency_uses_metadata_or_hashes() {
        let mut msg = msg_text("m2", "telegram", "Ping");
        // No metadata key → should hash deterministically
        let k1 = idempotency_key_or_hash(&msg);
        let k2 = idempotency_key_or_hash(&msg);
        assert_eq!(k1, k2, "hash fallback must be deterministic");

        // With metadata key → use it verbatim
        msg.metadata = json!({"idempotency_key":"custom-42"});
        let k3 = idempotency_key_or_hash(&msg);
        assert_eq!(k3, "custom-42");
    }

    // ---- Capabilities + inline schema validation (feature-gated) -----------
    #[test]
    fn validate_channel_data_inline_schema() {
        // Capabilities advertising the schema for channel_data
        let caps = ChannelCapabilities {
            name: "MS Teams".into(),
            version: "1.0.0".into(),
            supports_sending: true,
            supports_receiving: true,
            supports_text: true,
            supports_files: true,
            supports_media: true,
            supports_events: true,
            supports_typing: true,
            supports_routing: true,
            supports_threading: true,
            supports_reactions: true,
            supports_call: false,
            supports_buttons: true,
            supports_links: true,
            supports_custom_payloads: true,
            channel_data_schema_id: Some("greentic://channel/ms_teams.message@v1".into()),
            channel_data_schema: Some(JsonSchemaDescriptor::Inline {
                schema: json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "required": ["team_id","channel_id"],
                    "properties": {
                        "team_id": { "type": "string" },
                        "channel_id": { "type": "string" },
                        "message_id": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                id: Some("greentic://channel/ms_teams.message@v1".into()),
                version: Some("1".into()),
            }),
            supported_events: vec![],
        };

        let good = json!({"team_id":"t1","channel_id":"c1","message_id":"42"});
        let bad_missing = json!({"team_id":"t1"});
        let bad_extra = json!({"team_id":"t1","channel_id":"c1","xtra":true});

        // uses the helper in remote_plugin.rs
        assert!(validate_channel_data_with_caps(&caps, &good).is_ok());
        assert!(validate_channel_data_with_caps(&caps, &bad_missing).is_err());
        assert!(validate_channel_data_with_caps(&caps, &bad_extra).is_err());
    }

    // ---- Dummy handler exercising trait methods ----------------------------

    struct DummyHandler {
        caps: ChannelCapabilities,
        received: Arc<Mutex<Vec<ChannelMessage>>>,
        sent: Arc<Mutex<Vec<ChannelMessage>>>,
        refresh_calls: Arc<Mutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl RemotePluginHandler for DummyHandler {
        async fn capabilities(&self) -> anyhow::Result<ChannelCapabilities> {
            Ok(self.caps.clone())
        }

        async fn receive(&self, msg: ChannelMessage) -> anyhow::Result<()> {
            self.received.lock().unwrap().push(msg);
            Ok(())
        }

        async fn send(&self, msg: ChannelMessage) -> anyhow::Result<()> {
            self.sent.lock().unwrap().push(msg);
            Ok(())
        }

        async fn admin(&self, evt: AdminEvent) -> anyhow::Result<serde_json::Value> {
            match evt {
                AdminEvent::UpsertSubscription {
                    tenant, channel, ..
                } => Ok(json!({"ok":true,"tenant":tenant,"channel":channel})),
                AdminEvent::DeleteSubscription {
                    tenant, channel, ..
                } => Ok(json!({"ok":true,"deleted":true,"tenant":tenant,"channel":channel})),
                AdminEvent::UpsertToken { tenant, key, .. } => {
                    Ok(json!({"ok":true,"token_key":key,"tenant":tenant}))
                }
                AdminEvent::CancelToken { tenant, key } => {
                    Ok(json!({"ok":true,"cancelled":true,"token_key":key,"tenant":tenant}))
                }
                AdminEvent::GetCapabilities => Ok(json!(self.caps)),
            }
        }

        async fn refresh(&self, tenant: &str, key: &str) -> anyhow::Result<RefreshOutcome> {
            self.refresh_calls
                .lock()
                .unwrap()
                .push((tenant.to_string(), key.to_string()));
            // pretend we rescheduled 30 minutes from now
            Ok(RefreshOutcome::Rescheduled(
                SystemTime::now() + Duration::from_secs(1800),
            ))
        }
    }

    #[tokio::test]
    async fn dummy_handler_flow() {
        let caps = ChannelCapabilities {
            name: "Telegram".into(),
            version: "1.0.0".into(),
            supports_sending: true,
            supports_receiving: true,
            supports_text: true,
            supports_files: false,
            supports_media: false,
            supports_events: true,
            supports_typing: true,
            supports_routing: false,
            supports_threading: false,
            supports_reactions: false,
            supports_call: false,
            supports_buttons: true,
            supports_links: true,
            supports_custom_payloads: true,
            channel_data_schema: None,
            channel_data_schema_id: None,
            supported_events: vec![],
        };

        let handler = DummyHandler {
            caps,
            received: Arc::new(Mutex::new(vec![])),
            sent: Arc::new(Mutex::new(vec![])),
            refresh_calls: Arc::new(Mutex::new(vec![])),
        };

        // capabilities
        let got = handler.capabilities().await.unwrap();
        assert_eq!(got.name, "Telegram");

        // receive
        let m_in = msg_text("m-in", "telegram", "hi");
        handler.receive(m_in.clone()).await.unwrap();
        assert_eq!(handler.received.lock().unwrap().len(), 1);

        // send
        let mut m_out = msg_text("m-out", "telegram", "hello back");
        m_out.direction = MessageDirection::Outgoing;
        handler.send(m_out.clone()).await.unwrap();
        assert_eq!(handler.sent.lock().unwrap().len(), 1);

        // admin
        let res = handler
            .admin(AdminEvent::UpsertSubscription {
                tenant: "acme".into(),
                channel: "telegram".into(),
                params: json!({"chat_id": 123}),
            })
            .await
            .unwrap();
        assert_eq!(res["ok"], true);

        // refresh
        let outcome = handler.refresh("acme", "telegram:bot").await.unwrap();
        match outcome {
            RefreshOutcome::Rescheduled(t) => {
                assert!(t > SystemTime::now());
            }
            _ => panic!("expected rescheduled"),
        }
    }
}
