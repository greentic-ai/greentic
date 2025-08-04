use crate::models::{GraphNotification, ParentReference};
use channel_plugin::message::{ChannelMessage, Event, FileMetadata, MediaMetadata, MediaType, MessageContent, MessageDirection, Participant};
use serde_json::{json, Value};
use uuid::Uuid;

/// Converts a GraphNotification into a ChannelMessage (text or event).
pub fn to_channel_message(notif: &GraphNotification) -> Option<ChannelMessage> {
    let content = extract_content(notif)?;
    let now = chrono::Utc::now().to_rfc3339();

    let metadata = match serde_json::to_value(notif){
        Ok(value) => value,
        Err(_) => json!({}),
    };
    Some(ChannelMessage {
        id: Uuid::new_v4().to_string(),
        direction: MessageDirection::Incoming,
        channel: "ms_graph".to_string(),
        from: Participant {
            id: "ms_graph".to_string(),
            display_name: Some("Microsoft Graph".to_string()),
            channel_specific_id: None,
        },
        to: vec![],
        timestamp: now,
        content,
        session_id: Some(notif.subscription_id.clone()), // or use notif.resource?
        thread_id: None,
        reply_to_id: None,
        metadata,
    })
}


pub fn extract_content(notif: &GraphNotification) -> Option<Vec<MessageContent>> {
    let mut content = vec![];

    let resource = &notif.resource;

    if let Some(ref data) = notif.resource_data {
        // -------------------------------
        // 1. Teams or Outlook text message
        // -------------------------------
        if let Some(body) = &data.body {
            content.push(MessageContent::Text {
                text: body.content.clone(),
            });
        }

        // -------------------------------
        // 2. SharePoint / OneDrive file
        // -------------------------------
        if let Some(name) = &data.name {
            if let Some(url) = extract_file_url(&data.parent_reference, name) {
                let mime_type = infer_mime_type(name);

                let file_meta = FileMetadata {
                    file_name: name.clone(),
                    mime_type: mime_type.clone(),
                    url,
                    size_bytes: None, // Add size if available later
                };

                content.push(match classify_media(&mime_type) {
                    Some(kind) => MessageContent::Media {
                        media: MediaMetadata { kind, file: file_meta },
                    },
                    None => MessageContent::File { file: file_meta },
                });
            }
        }

        // -------------------------------
        // 3. Calendar events
        // -------------------------------
        if resource.contains("/events") {
            content.push(MessageContent::Event {
                event: Event{
                    event_type: "calendar_event".to_string(),
                    event_payload: serde_json::to_value(data).unwrap_or(Value::Null),
                }
            });
        }

        // -------------------------------
        // 4. Fallback event
        // -------------------------------
        if content.is_empty() {
            content.push(MessageContent::Event {
                event: Event{
                    event_type: "ms_graph_event".to_string(),
                    event_payload: serde_json::to_value(data).unwrap_or(Value::Null),
                }
            });
        }
    } else {
        // Entirely missing resource_data fallback
        content.push(MessageContent::Event {
            event: Event{
                event_type: "ms_graph_event".to_string(),
                event_payload: serde_json::to_value(notif).unwrap_or(Value::Null),
            }
        });
    }

    Some(content)
}

fn extract_file_url(parent: &Option<ParentReference>, name: &str) -> Option<String> {
    if let Some(p) = parent {
        if let Some(parent_path) = &p.path {
            return Some(format!("{}/{}", parent_path.trim_end_matches('/'), name));
        }
    }
    None
}

fn infer_mime_type(file_name: &str) -> String {
    let ext = file_name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

pub fn classify_media(mime: &str) -> Option<MediaType> {
    if mime.starts_with("image/") {
        Some(MediaType::IMAGE)
    } else if mime.starts_with("video/") {
        Some(MediaType::VIDEO)
    } else if mime.starts_with("audio/") {
        Some(MediaType::AUDIO)
    } else {
        None
    }
}

pub fn extract_event_type(message: &ChannelMessage) -> Option<&str> {
    for content in &message.content {
        if let MessageContent::Event { event } = content {
            return Some(event.event_type.as_str());
        }
    }
    None
}