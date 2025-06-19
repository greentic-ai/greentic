use chrono::{DateTime, Utc};
use dashmap::DashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum RouteMatcher {
    /// Match based on exact Telegram command (e.g., `/start`)
    Command(String),
    /// Match based on reply thread/session ID
    ThreadId(String),
    /// Match all messages from a participant
    Participant(String),
    /// Match by webhook path
    WebPath(String),
    /// Match by custom logic (used only internally or via pattern)
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RouteBinding {
    pub flow: String,
    pub node: String,
    pub matcher: RouteMatcher,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ChannelMessage {
    pub id: String,                      // Unique ID (UUID or channel-provided)
    pub flow: Option<String>,            // The flow to route to
    pub node: Option<String>,            // The node inside the flow to route to
    pub session_id: Option<String>,      // A UUID uses during the same flow execution by the same user
    pub direction: MessageDirection,     // Incoming or Outgoing
    pub timestamp: DateTime<Utc>,        // When it was sent or received
    pub channel: String,                 // Slack, Telegram, Email, etc.
    pub from: Participant,               // Sender info
    pub to: Vec<Participant>,            // Recipient(s)

    pub content: Option<MessageContent>, // Text or attachment
    pub thread_id: Option<String>,       // For threading support
    pub reply_to_id: Option<String>,     // If replying to another message
    #[schemars(with = "HashMap<String, String>")]  // same for secrets
    pub metadata: DashMap<String, Value>, // Channel-specific or custom data
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub enum MessageDirection {
    #[default]
    Incoming,
    Outgoing
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
pub struct Participant {
    pub id: String,                      // Internal or platform-specific ID
    pub display_name: Option<String>,   // Optional for SMS, Email
    pub channel_specific_id: Option<String>, // E.g., phone number, email, handle
}

impl Participant {
    pub fn new(id: String, display_name: Option<String>, channel_specific_id: Option<String>) -> Self {
        Self{id, display_name, channel_specific_id}
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum MessageContent {
    Text(String),
    File(FileMetadata),
    Media(MediaMetadata),
    Event(Event),
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct FileMetadata {
    pub file_name: String,
    pub mime_type: String,
    pub url: String,                  // Direct link or temporary
    pub size_bytes: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct MediaMetadata {
    pub kind: MediaType,             // Image, Video, Audio
    pub file: FileMetadata,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum MediaType {
    Image,
    Video,
    Audio,
    Binary
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Event {
    pub event_type: String,           
    pub event_payload: Option<Value> 
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EventType {
    pub event_type: String,             // e.g., "UserJoined", "TypingStarted"
    pub description: String,            // Human-readable description
    pub payload_schema: Option<Value>,  // the json schema for the event_payload  
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ChannelCapabilities {
    pub name: String,                         // e.g. "Slack", "Email", "SMS"
    pub supports_sending: bool,
    pub supports_receiving: bool,
    pub supports_text: bool,
    pub supports_files: bool,
    pub supports_media: bool,
    pub supports_events: bool,
    pub supports_typing: bool,
    pub supports_routing: bool,
    pub supports_threading: bool,
    pub supports_reactions: bool,
    pub supports_call: bool,
    pub supports_buttons: bool,
    pub supports_links: bool,
    pub supports_custom_payloads: bool,

    pub supported_events: Vec<EventType>, // List of events with descriptions
}

/// Can be used to route messaging platforms:
/// - commands
/// - thread_id when messages are in a thread
/// - participant_id
/// - chat_type (e.g. public, private)
/// - is_reply_to_bot
/// - language_code
/// - custom e.g., ["lang:en", "chat_type:private"]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct MessagingRouteContext {
    pub command: Option<String>,
    pub thread_id: Option<String>,
    pub participant_id: Option<String>,
    pub chat_type: Option<String>,
    pub is_reply_to_bot: bool,
    pub language_code: Option<String>,
    pub custom: Vec<String>,  // e.g., ["lang:en", "chat_type:private"]
}

impl MessagingRouteContext {
    pub fn to_matchers(&self) -> Vec<RouteMatcher> {
        let mut matchers = Vec::new();

        if let Some(c) = &self.command {
            matchers.push(RouteMatcher::Command(c.clone()));
        }

        if let Some(tid) = &self.thread_id {
            matchers.push(RouteMatcher::ThreadId(tid.clone()));
        }

        if let Some(pid) = &self.participant_id {
            matchers.push(RouteMatcher::Participant(pid.clone()));
        }

        for c in &self.custom {
            matchers.push(RouteMatcher::Custom(c.clone()));
        }

        matchers
    }
}


/// Can be used to define http type of routing with a WebPath
/// or Participant (e.g. session in websockets)
/// or Custom
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct HttpRouteContext {
    pub path: Option<String>,
    pub participant_id: Option<String>,
    pub thread_id: Option<String>,
    pub custom: Vec<String>,
}

impl HttpRouteContext {
    pub fn to_matchers(&self) -> Vec<RouteMatcher> {
        let mut v = Vec::new();

        if let Some(pid) = &self.participant_id {
            v.push(RouteMatcher::Participant(pid.clone()));
        }

        if let Some(p) = &self.path {
            v.push(RouteMatcher::WebPath(p.clone()));
        }

        for c in &self.custom {
            v.push(RouteMatcher::Custom(c.clone()));
        }

        v
    }
}

pub const USER_JOINED: &str = "UserJoined";
pub const USER_LEFT: &str = "UserLeft";


pub fn build_user_joined_event(channel: &str, user_id: &str, session_id: Option<String>) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        channel: channel.into(),
        node: None,
        flow: None,
        session_id: session_id.clone(),
        direction: MessageDirection::Incoming,
        from: Participant {
            id: user_id.into(),
            display_name: None,
            channel_specific_id: None,
        },
        to: vec![],
        thread_id: None,
        reply_to_id: None,
        content: Some(MessageContent::Event(Event{
            event_type: USER_JOINED.to_string(),
            event_payload: Some(json!({ "user_id": user_id })),
        })),
        metadata: DashMap::new(),
    }
}

pub fn build_user_left_event(channel: &str, user_id: &str, session_id: Option<String>) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        channel: channel.into(),
        node: None,
        flow: None,
        session_id: session_id.clone(),
        direction: MessageDirection::Incoming,
        from: Participant {
            id: user_id.into(),
            display_name: None,
            channel_specific_id: None,
        },
        to: vec![],
        thread_id: None,
        reply_to_id: None,
        content: Some(MessageContent::Event(Event{
            event_type: USER_LEFT.to_string(),
            event_payload: Some(json!({ "user_id": user_id })),
        })),
        metadata: DashMap::new(),
    }
}

pub fn get_user_joined_left_events() -> Vec<EventType> {
    vec![
        EventType {
            event_type: USER_JOINED.to_string(),
            description: "Event sent when a user connects".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "properties": {
                    "user_id": {
                        "type": "string",
                        "format": "channel specific unique user_id",
                        "description": "A channel specific and potentially only session unique user_id"
                    }
                },
                "required": ["user_id"]
            })),
        },
        EventType {
            event_type: USER_LEFT.to_string(),
            description: "Event sent when a user disconnects".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "properties": {
                    "user_id": {
                        "type": "string",
                        "format": "channel specific unique user_id",
                        "description": "A channel specific and potentially only session unique user_id"
                    }
                },
                "required": ["user_id"]
            })),
        },
    ]
}

#[derive(Clone, Debug, Serialize, Deserialize,)]
pub struct TextMessage {
    pub text: String
}