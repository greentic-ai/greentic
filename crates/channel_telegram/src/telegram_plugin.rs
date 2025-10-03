// channel_telegram/src/lib.rs
use anyhow::anyhow;
use async_trait::async_trait;
use channel_plugin::{
    message::{
        CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult, Event,
        EventType, FileMetadata, HealthResult, InitResult, ListKeysResult, MediaMetadata,
        MediaType, MessageContent, MessageDirection, MessageInResult, MessageOutParams,
        MessageOutResult, NameResult, PLUGIN_VERSION, Participant, StateResult, StopResult,
        TextMessage, make_session_key,
    },
    plugin_helpers::{build_user_joined_event, get_user_joined_left_events},
    plugin_runtime::{HasStore, PluginHandler},
};
use chrono::Utc;
use crossbeam::channel::{Receiver, Sender, unbounded};
use dashmap::DashMap;
use once_cell::sync::OnceCell;
use reqwest::Url;
use serde_json::json;
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    thread,
};
use teloxide::{
    prelude::*,
    types::{InputFile, MediaKind, Message as TelegramMessage, ParseMode},
    utils::command::BotCommands,
};
use tokio::{runtime::Builder, sync::Notify};
use tracing::{error, info};

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Bot commands")]
enum Command {
    #[command(description = "Start the bot")]
    Start,
}

/// Extract `MessageContent` from a Telegram SDK message
fn extract_content(bot: Bot, msg: &TelegramMessage) -> Option<MessageContent> {
    use teloxide::types::MessageKind;

    if let MessageKind::Common(common) = &msg.kind {
        match &common.media_kind {
            MediaKind::Text(t) => Some(MessageContent::Text {
                text: t.text.clone(),
            }),

            MediaKind::Photo(ph) => {
                let photo = ph.photo.last()?;
                Some(MessageContent::Media {
                    media: MediaMetadata {
                        kind: MediaType::IMAGE,
                        file: FileMetadata {
                            file_name: "photo.jpg".into(),
                            mime_type: "image/jpeg".into(),
                            url: format!(
                                "https://api.telegram.org/file/bot{}/{}",
                                bot.token(),
                                photo.file.id
                            ),
                            size_bytes: Some(photo.file.size as u64),
                        },
                    },
                })
            }

            MediaKind::Video(video) => Some(MessageContent::Media {
                media: MediaMetadata {
                    kind: MediaType::VIDEO,
                    file: FileMetadata {
                        file_name: video.video.file_name.clone().unwrap_or("video.mp4".into()),
                        mime_type: video
                            .video
                            .mime_type
                            .as_ref()
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| "video/mp4".into()),
                        url: format!(
                            "https://api.telegram.org/file/bot{}/{}",
                            bot.token(),
                            video.video.file.id
                        ),
                        size_bytes: Some(video.video.file.size as u64),
                    },
                },
            }),

            MediaKind::Voice(voice) => Some(MessageContent::Media {
                media: MediaMetadata {
                    kind: MediaType::AUDIO,
                    file: FileMetadata {
                        file_name: "voice.ogg".into(),
                        mime_type: "audio/ogg".into(),
                        url: format!(
                            "https://api.telegram.org/file/bot{}/{}",
                            bot.token(),
                            voice.voice.file.id
                        ),
                        size_bytes: Some(voice.voice.file.size as u64),
                    },
                },
            }),

            MediaKind::Audio(audio) => Some(MessageContent::Media {
                media: MediaMetadata {
                    kind: MediaType::AUDIO,
                    file: FileMetadata {
                        file_name: audio.audio.title.clone().unwrap_or("audio.mp3".into()),
                        mime_type: audio
                            .audio
                            .mime_type
                            .as_ref()
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| "video/mp4".into()),
                        url: format!(
                            "https://api.telegram.org/file/bot{}/{}",
                            bot.token(),
                            audio.audio.file.id
                        ),
                        size_bytes: Some(audio.audio.file.size as u64),
                    },
                },
            }),

            MediaKind::Document(doc) => Some(MessageContent::File {
                file: FileMetadata {
                    file_name: doc.document.file_name.clone().unwrap_or("file".into()),
                    mime_type: doc
                        .document
                        .mime_type
                        .as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "video/mp4".into()),
                    url: format!(
                        "https://api.telegram.org/file/bot{}/{}",
                        bot.token(),
                        doc.document.file.id
                    ),
                    size_bytes: Some(doc.document.file.size as u64),
                },
            }),

            MediaKind::Sticker(sticker) => Some(MessageContent::Media {
                media: MediaMetadata {
                    kind: MediaType::IMAGE,
                    file: FileMetadata {
                        file_name: "sticker.webp".into(),
                        mime_type: "image/webp".into(),
                        url: format!(
                            "https://api.telegram.org/file/bot{}/{}",
                            bot.token(),
                            sticker.sticker.file.id
                        ),
                        size_bytes: Some(sticker.sticker.file.size as u64),
                    },
                },
            }),

            MediaKind::VideoNote(video_note) => Some(MessageContent::Media {
                media: MediaMetadata {
                    kind: MediaType::VIDEO,
                    file: FileMetadata {
                        file_name: "video_note.mp4".into(),
                        mime_type: "video/mp4".into(),
                        url: format!(
                            "https://api.telegram.org/file/bot{}/{}",
                            bot.token(),
                            video_note.video_note.file.id
                        ),
                        size_bytes: Some(video_note.video_note.file.size as u64),
                    },
                },
            }),

            MediaKind::Animation(anim) => Some(MessageContent::Media {
                media: MediaMetadata {
                    kind: MediaType::VIDEO,
                    file: FileMetadata {
                        file_name: anim
                            .animation
                            .file_name
                            .clone()
                            .unwrap_or("animation.mp4".into()),
                        mime_type: anim
                            .animation
                            .mime_type
                            .as_ref()
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| "video/mp4".into()),
                        url: format!(
                            "https://api.telegram.org/file/bot{}/{}",
                            bot.token(),
                            anim.animation.file.id
                        ),
                        size_bytes: Some(anim.animation.file.size as u64),
                    },
                },
            }),

            MediaKind::Contact(contact) => Some(MessageContent::Event {
                event: Event {
                    event_type: "ContactShared".into(),
                    event_payload: serde_json::json!({
                        "name": format!("{} {}", contact.contact.first_name, contact.contact.last_name.clone().unwrap_or_default()),
                        "phone_number": contact.contact.phone_number
                    }),
                },
            }),

            MediaKind::Location(loc) => Some(MessageContent::Event {
                event: Event {
                    event_type: "LocationShared".into(),
                    event_payload: serde_json::json!({
                        "latitude": loc.location.latitude,
                        "longitude": loc.location.longitude
                    }),
                },
            }),

            MediaKind::Venue(venue) => Some(MessageContent::Event {
                event: Event {
                    event_type: "VenueShared".into(),
                    event_payload: serde_json::json!({
                        "title": venue.venue.title,
                        "address": venue.venue.address,
                        "location": {
                            "lat": venue.venue.location.latitude,
                            "lon": venue.venue.location.longitude,
                        }
                    }),
                },
            }),

            _ => None,
        }
    } else {
        None
    }
}

/// Our plugin struct holds just the minimal shared state.
#[derive(Debug, Clone)]
pub struct TelegramPlugin {
    /// Incoming queue for async receive_message
    incoming_tx: Sender<ChannelMessage>,
    inbound_rx: Receiver<ChannelMessage>,
    state: Arc<Mutex<ChannelState>>,
    bot: Option<Bot>,
    cfg: DashMap<String, String>,
    secrets: DashMap<String, String>,
    shutdown: Option<Arc<tokio::sync::Notify>>,
}

impl Default for TelegramPlugin {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        TelegramPlugin {
            incoming_tx: tx,
            inbound_rx: rx,
            state: Arc::new(Mutex::new(ChannelState::STOPPED)),
            bot: None,
            cfg: DashMap::new(),
            secrets: DashMap::new(),
            shutdown: None,
        }
    }
}

impl HasStore for TelegramPlugin {
    fn config_store(&self) -> &DashMap<String, String> {
        &self.cfg
    }
    fn secret_store(&self) -> &DashMap<String, String> {
        &self.secrets
    }
}

/// We assume that the telegram bot token is set via a secret called "telegram_token"
/// We also assume that when sending a message the participant id is the same as the chat_id.
impl TelegramPlugin {
    /// Spawn a background dispatcher if not already running.
    async fn init_dispatcher(&mut self) {
        let notify = Arc::new(Notify::new());
        self.shutdown = Some(notify.clone());
        static STARTED: OnceCell<()> = OnceCell::new();
        if STARTED.set(()).is_ok() {
            // 1) Grab the token & build the Bot
            let token = match self.secrets.get("TELEGRAM_TOKEN") {
                Some(entry) => entry.value().clone(),
                None => String::new(),
            };
            let bot = Bot::new(token);
            self.bot = Some(bot.clone());
            // 2) Clone our plugin‐inbound channel & logger
            let tx = self.incoming_tx.clone();
            let plugin = self.name().clone();
            // 3) Build a dptree handler that fires on Message updates
            let handler = Update::filter_message()
                .branch(
                    teloxide::filter_command::<Command, Result<(), Infallible>>().branch(
                        dptree::case![Command::Start].endpoint({
                            let tx = tx.clone();
                            move |_bot: Bot, msg: Message| {
                                // Clone inside so closure can be reused
                                let tx = tx.clone();
                                handle_start_command(msg, tx, "telegram")
                            }
                        }),
                    ),
                )
                .endpoint(move |bot: Bot, msg: Message| {
                    let tx = tx.clone();
                    let plugin = plugin.clone();
                    async move {
                        if let Some(content) = extract_content(bot, &msg) {
                            let chat_id = msg.chat.id.to_string();
                            let user_id = msg.from.clone().expect("No user id").id.to_string();
                            let thread_id: Option<String> = match &msg.thread_id {
                                Some(tid) => Some(tid.0.0.to_string()), // ThreadId.0 is MessageId, MessageId.0 is i32
                                None => None,
                            };
                            let reply_to_id: Option<String> = msg
                                .reply_to_message()
                                .and_then(|reply| reply.from.as_ref())
                                .map(|user| user.id.0.to_string());
                            let key = format!("chat:{}:user:{}", chat_id, user_id.clone());
                            let session_id = make_session_key(&plugin.name, &key);

                            let cm = ChannelMessage {
                                channel: "telegram".into(),
                                channel_data: json![{}],
                                session_id: Some(session_id.clone()),
                                direction: MessageDirection::Incoming,
                                from: Participant {
                                    id: user_id,
                                    display_name: msg.from.as_ref().map(|u| u.full_name()),
                                    channel_specific_id: msg
                                        .from
                                        .as_ref()
                                        .and_then(|u| u.username.clone()),
                                },
                                content: vec![content],
                                id: msg.id.clone().to_string(),
                                timestamp: Utc::now().to_rfc3339(),
                                to: Vec::new(),
                                thread_id,
                                reply_to_id,
                                metadata: Default::default(),
                            };

                            if let Err(e) = tx.send(cm) {
                                error!("queue send error: {}", e);
                            }
                        }
                        // dptree requires an Ok(()) return
                        Ok::<(), Infallible>(())
                    }
                });
            // 4) Spawn the dispatcher on the Tokio runtime
            let notify_for_task = notify.clone();
            thread::spawn(move || {
                let rt = Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build Tokio runtime for Telegram");
                rt.block_on(async move {
                    let mut dispatcher = Dispatcher::builder(bot, handler).build();
                    tokio::select! {
                        _ = dispatcher.dispatch() => {
                            error!("Telegram dispatcher exited");
                        }
                        _ = notify_for_task.notified() => {
                            info!("Telegram dispatcher shutting down");
                        }
                    }
                });
            });
        }
    }
}

#[async_trait]
impl PluginHandler for TelegramPlugin {
    fn name(&self) -> NameResult {
        NameResult {
            name: "telegram".to_string(),
        }
    }

    fn capabilities(&self) -> CapabilitiesResult {
        let mut events = get_user_joined_left_events();
        events.push(EventType {
            event_type: "ContactShared".into(),
            description: "User shared a contact".into(),
            payload_schema: Some(json!({
                "type": "object",
                "properties": {
                    "name":         { "type": "string" },
                    "phone_number": { "type": "string" }
                },
                "required": ["name", "phone_number"]
            })),
        });
        events.push(EventType {
            event_type: "LocationShared".into(),
            description: "User shared a location".into(),
            payload_schema: Some(json!({
                "type": "object",
                "properties": {
                    "latitude":  { "type": "number" },
                    "longitude": { "type": "number" }
                },
                "required": ["latitude", "longitude"]
            })),
        });
        events.push(EventType {
            event_type: "VenueShared".into(),
            description: "User shared a venue".into(),
            payload_schema: Some(json!({
                "type": "object",
                "properties": {
                    "title":   { "type": "string" },
                    "address": { "type": "string" },
                    "location": {
                        "type": "object",
                        "properties": {
                            "lat": { "type": "number" },
                            "lon": { "type": "number" }
                        },
                        "required": ["lat", "lon"]
                    }
                },
                "required": ["title", "address", "location"]
            })),
        });
        CapabilitiesResult {
            capabilities: ChannelCapabilities {
                name: "telegram".into(),
                version: PLUGIN_VERSION.to_string(),
                supports_sending: true,
                supports_receiving: true,
                supports_text: true,
                supports_files: true,
                supports_media: true,
                supports_events: true,
                supports_typing: true,
                supports_threading: false,
                supports_routing: true,
                supports_reactions: false,
                supports_call: false,
                supports_buttons: false,
                supports_links: true,
                supports_custom_payloads: false,
                channel_data_schema: None,
                channel_data_schema_id: None,
                supported_events: events,
            },
        }
    }

    fn list_config_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![],
            optional_keys: vec![],
            dynamic_keys: vec![],
        }
    }

    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult{
            required_keys: vec![
                ("TELEGRAM_TOKEN".to_string(),
                Some("The token you get from BotFather when you create a new bot. Go to https://telegram.me/botfather to get one.".to_string()))
            ],
            optional_keys: vec![],
            dynamic_keys: vec![],
        }
    }

    async fn health(&self) -> HealthResult {
        HealthResult {
            healthy: true,
            reason: None,
        }
    }
    async fn state(&self) -> StateResult {
        StateResult {
            state: self.state.lock().unwrap().clone(),
        }
    }

    async fn init(&mut self, _p: channel_plugin::message::InitParams) -> InitResult {
        info!("[telegram] init");
        self.init_dispatcher().await;
        *self.state.lock().unwrap() = ChannelState::RUNNING;
        InitResult {
            success: true,
            error: None,
        }
    }

    async fn drain(&mut self) -> DrainResult {
        info!("[telegram] drain called");
        if let Some(notify) = &self.shutdown {
            notify.notify_one();
        }

        // Since we use an unbounded channel for incoming messages, and
        // our send_message never blocks, there's nothing to wait for on drain.
        // If you had an outbound queue, you'd await that here.
        *self.state.lock().unwrap() = ChannelState::STOPPED;
        DrainResult {
            success: true,
            error: None,
        }
    }

    async fn stop(&mut self) -> StopResult {
        info!("[telegram] stop called");
        if let Some(notify) = &self.shutdown {
            notify.notify_one();
        }
        *self.state.lock().unwrap() = ChannelState::STOPPED;
        StopResult {
            success: true,
            error: None,
        }
    }

    async fn send_message(&mut self, msg: MessageOutParams) -> MessageOutResult {
        info!("[telegram] Sending message: {:?}", msg.message.id);

        // 1) pull off the Vec<Participant> and the Option<String> by cloning them:
        let to_list = msg.message.to.clone();
        //let runtime_handle = self.runtime.handle().clone();
        // 2) now you can still use `msg` freely; whenever you call send_msg, clone `msg`:
        if to_list.is_empty() {
            let error = "sending to empty participant is not possible";
            error!(error);
            return MessageOutResult {
                success: false,
                error: Some(error.into()),
            };
        } else {
            let Some(bot) = self.bot.clone() else {
                let error = "bot not initialised for telegram";
                error!(error);
                return MessageOutResult {
                    success: false,
                    error: Some(error.into()),
                };
            };
            match tokio::spawn(async move {
                send_telegram_to_list(&bot, &to_list, &msg.message.content).await
            })
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => MessageOutResult {
                    success: false,
                    error: Some(error.to_string()),
                },
                Err(error) => MessageOutResult {
                    success: false,
                    error: Some(error.to_string()),
                },
            }
        }
    }

    /// Telegram messages get routed via:
    /// - participants -> set a Participant route for a telegram id
    /// - commands -> set a Command route
    /// - thread_id -> set a ThreadId route
    /// - reply_to_bot -> reply_to_bot:true
    async fn receive_message(&mut self) -> MessageInResult {
        match self.inbound_rx.recv() {
            Ok(msg) => {
                info!("[telegram] message_in {}", msg.id);
                MessageInResult {
                    message: msg,
                    error: false,
                }
            }
            Err(err) => {
                error!("[telegram] empty message_in: {:?}", err);
                // The sending half was dropped – return an “empty” answer so
                // the runtime doesn’t panic. (You could also shut the plugin
                // down here.)
                MessageInResult {
                    message: ChannelMessage::default(), // tiny helper shown earlier
                    error: true,
                }
            }
        }
    }
}

pub async fn send_telegram_to_list(
    bot: &Bot,
    recipients: &[Participant],
    contents: &[MessageContent],
) -> Result<MessageOutResult, anyhow::Error> {
    // Iterate over every recipient
    for rcpt in recipients {
        let chat_id = rcpt.id.clone();
        info!(target: "telegram", "Sending to {chat_id}");

        if contents.len() == 0 {
            return Ok(MessageOutResult {
                success: false,
                error: Some("Content was empty".to_string()),
            });
        }
        // Send every content item for this recipient
        for content in contents {
            let result = match content {
                // ------------- TEXT --------------------------------------------------------
                MessageContent::Text { text } => {
                    // Allow optional JSON‐wrapped `{ "text": "..."}`
                    let txt = serde_json::from_str::<TextMessage>(text)
                        .map(|tm| tm.text)
                        .unwrap_or_else(|_| text.clone());

                    bot.send_message(chat_id.clone(), txt)
                        .parse_mode(ParseMode::Html)
                        .send()
                        .await
                        .map(|_| ())
                        .map_err(|e| anyhow!("telegram send text: {e}"))
                }

                // ------------- FILE --------------------------------------------------------
                MessageContent::File { file } => {
                    let url = Url::parse(&file.url).map_err(|e| anyhow!("bad file URL: {e}"))?;
                    let input = InputFile::url(url).file_name(file.file_name.clone());

                    bot.send_document(chat_id.clone(), input)
                        .send()
                        .await
                        .map(|_| ())
                        .map_err(|e| anyhow!("telegram send file: {e}"))
                }

                // ------------- MEDIA -------------------------------------------------------
                MessageContent::Media { media } => {
                    let url =
                        Url::parse(&media.file.url).map_err(|e| anyhow!("bad media URL: {e}"))?;
                    let input = InputFile::url(url).file_name(media.file.file_name.clone());

                    use MediaType::*;
                    let fut = match media.kind {
                        IMAGE => bot.send_photo(chat_id.clone(), input).send().await,
                        VIDEO => bot.send_video(chat_id.clone(), input).send().await,
                        AUDIO | BINARY => bot.send_audio(chat_id.clone(), input).send().await,
                    };

                    fut.map(|_| ())
                        .map_err(|e| anyhow!("telegram send media: {e}"))
                }

                // ------------- EVENT (fallback: pretty-printed JSON) -----------------------
                MessageContent::Event { event } => {
                    let body = serde_json::to_string_pretty(event)
                        .unwrap_or_else(|_| format!("event: {}", event.event_type));
                    bot.send_message(chat_id.clone(), body)
                        .send()
                        .await
                        .map(|_| ())
                        .map_err(|e| anyhow!("telegram send event: {e}"))
                }
            };

            // If any content chunk fails, log & propagate up.
            if let Err(e) = result {
                error!(target: "telegram", "send error: {e:?}");
                return Err(e);
            }
        }
    }

    info!(target: "telegram", "all messages delivered");
    Ok(MessageOutResult {
        success: true,
        error: None,
    })
}

async fn handle_start_command(
    msg: Message,
    tx: Sender<ChannelMessage>,
    plugin: &str,
) -> Result<(), Infallible> {
    info!("/start command received");

    let chat_id = msg.chat.id.to_string();
    let user_id = msg.from.clone().expect("No user id").id.to_string();

    let key = format!("chat:{}:user:{}", chat_id, user_id.clone());
    let session_id = make_session_key(&plugin, &key);

    let cm = build_user_joined_event("telegram", &user_id, Some(session_id));
    if let Err(e) = tx.send(cm) {
        error!("queue send error: {}", e);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use channel_plugin::{
        message::{
            ChannelMessage, InitParams, LogLevel, MessageContent, MessageDirection, Participant,
            WaitUntilDrainedParams,
        },
        plugin_helpers::load_env_as_vecs,
    };
    use chrono::Utc;

    #[tokio::test]
    async fn test_state_transitions_async() {
        let (config, secrets) = load_env_as_vecs(
            Some("../../greentic/secrets/.env"),
            None, /* default: .env in cwd */
        )
        .expect("failed to read .env");
        let mut p = TelegramPlugin::default();

        assert_eq!(
            p.state().await,
            StateResult {
                state: ChannelState::STOPPED
            }
        );

        assert!(
            p.start(InitParams {
                version: "123".to_string(),
                config,
                secrets,
                log_level: LogLevel::Info,
                log_dir: Some("./logs".to_string()),
                otel_endpoint: None
            })
            .await
            .success
        );
        assert_eq!(
            p.state().await,
            StateResult {
                state: ChannelState::RUNNING
            }
        );

        p.drain().await;
        assert_eq!(
            p.state().await,
            StateResult {
                state: ChannelState::STOPPED
            }
        );

        p.stop().await;
        assert_eq!(
            p.state().await,
            StateResult {
                state: ChannelState::STOPPED
            }
        );
    }

    #[test]
    fn test_capabilities() {
        let p = TelegramPlugin::default();
        let caps = p.capabilities();
        assert_eq!(caps.capabilities.name, "telegram");
        assert!(caps.capabilities.supports_text);
        assert!(caps.capabilities.supports_media);
        assert!(!caps.capabilities.supports_call);
    }

    #[tokio::test]
    async fn test_send_without_content_errors_async() {
        let mut p = TelegramPlugin::default();

        // Test that we expect a telegram token
        assert!(
            !p.start(InitParams {
                version: "123".to_string(),
                config: vec![],
                secrets: vec![],
                log_level: LogLevel::Info,
                log_dir: Some("../../logs".to_string()),
                otel_endpoint: None
            })
            .await
            .success
        );

        assert!(
            p.start(InitParams {
                version: "123".to_string(),
                config: vec![],
                secrets: vec![("TELEGRAM_TOKEN".to_string(), "123".to_string())],
                log_level: LogLevel::Info,
                log_dir: Some("./logs".to_string()),
                otel_endpoint: None
            })
            .await
            .success
        );

        // default msg has no content
        let mut msg = ChannelMessage::default();
        msg.to = vec![Participant {
            id: "123".into(),
            display_name: None,
            channel_specific_id: None,
        }];

        // should err without a text
        assert!(
            !p.send_message(MessageOutParams { message: msg })
                .await
                .success
        );
    }

    #[tokio::test]
    async fn test_send_and_receive_roundtrip_async() {
        let mut p = TelegramPlugin::default();
        assert!(
            p.init(InitParams {
                version: "123".to_string(),
                config: vec![],
                secrets: vec![("TELEGRAM_TOKEN".to_string(), "fake".to_string())],
                log_level: LogLevel::Info,
                log_dir: Some("./logs".to_string()),
                otel_endpoint: None
            })
            .await
            .success
        );
        // Simulate an incoming Telegram message
        let incoming = ChannelMessage {
            id: "in1".into(),
            session_id: Some("chat42".into()),
            direction: MessageDirection::Incoming,
            timestamp: Utc::now().to_rfc3339(),
            channel: "telegram".into(),
            channel_data: json!({}),
            from: Participant {
                id: "chat42".into(),
                display_name: None,
                channel_specific_id: None,
            },
            to: vec![],
            content: vec![MessageContent::Text {
                text: "hello".into(),
            }],
            thread_id: None,
            reply_to_id: None,
            metadata: Default::default(),
        };

        // Manually push into the plugin's incoming channel
        let _ = p.incoming_tx.send(incoming.clone());

        // receive it
        let got = p.receive_message().await;
        assert_eq!(got.message.id, "in1");
        assert_eq!(
            got.message.content,
            vec![MessageContent::Text {
                text: "hello".into()
            }]
        );

        // Test send_message paths (won't actually call Telegram)
        let outgoing = ChannelMessage {
            id: "out1".into(),
            session_id: Some("chat42".into()),
            direction: MessageDirection::Outgoing,
            timestamp: Utc::now().to_rfc3339(),
            channel: "telegram".into(),
            channel_data: json!({}),
            from: Participant {
                id: "bot".into(),
                display_name: None,
                channel_specific_id: None,
            },
            to: vec![Participant {
                id: "chat42".into(),
                display_name: None,
                channel_specific_id: None,
            }],
            content: vec![MessageContent::Text {
                text: "reply".into(),
            }],
            thread_id: None,
            reply_to_id: None,
            metadata: Default::default(),
        };

        // send_message should fail fast on bad token
        assert!(
            !p.send_message(MessageOutParams { message: outgoing })
                .await
                .success
        );
    }

    #[tokio::test]
    async fn test_wait_until_drained_async() {
        let mut p = TelegramPlugin::default();
        assert!(
            p.init(InitParams {
                version: "123".to_string(),
                config: vec![],
                secrets: vec![("TELEGRAM_TOKEN".to_string(), "fake".to_string())],
                log_level: LogLevel::Info,
                log_dir: Some("./logs".to_string()),
                otel_endpoint: None
            })
            .await
            .success
        );
        p.drain().await;

        // Since we have no backlog, this should return immediately
        assert!(
            p.wait_until_drained(WaitUntilDrainedParams { timeout_ms: 10 })
                .await
                .stopped
        );
    }

    #[tokio::test]
    async fn test_set_config_and_secrets_async() {
        let mut p = TelegramPlugin::default();
        assert!(
            p.start(InitParams {
                version: "123".to_string(),
                config: vec![("foo".to_string(), "bar".to_string())],
                secrets: vec![("TELEGRAM_TOKEN".to_string(), "token".to_string())],
                log_level: LogLevel::Info,
                log_dir: Some("./logs".to_string()),
                otel_endpoint: None
            })
            .await
            .success
        );
        let entry = p.get_config("foo").expect("`foo` must exist");
        assert_eq!(&entry, "bar");
        let entry = p
            .get_secret("TELEGRAM_TOKEN")
            .expect("telegra token not set");
        assert_eq!(&entry, "token");
    }
}
