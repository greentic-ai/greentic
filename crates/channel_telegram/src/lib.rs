// channel_telegram/src/lib.rs

use std::convert::Infallible;
use async_trait::async_trait;
use dashmap::DashMap;
use chrono::Utc;
use once_cell::sync::OnceCell;
use teloxide::{
    prelude::*,
    types::{MediaKind, Message as TelegramMessage},
};
use channel_plugin::{
    export_plugin,
    message::{
        ChannelCapabilities, ChannelMessage, Event, FileMetadata, MediaMetadata, MediaType, MessageContent, MessageDirection, Participant
    },
    plugin::{ChannelPlugin, ChannelState, LogLevel, PluginError, PluginLogger},
};
use tokio::{sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender}};

/// Extract `MessageContent` from a Telegram SDK message
fn extract_content(bot: Bot, msg: &TelegramMessage) -> Option<MessageContent> {
    use teloxide::types::MessageKind;

    if let MessageKind::Common(common) = &msg.kind {
        match &common.media_kind {
            MediaKind::Text(t) => Some(MessageContent::Text(t.text.clone())),

            MediaKind::Photo(ph) => {
                let photo = ph.photo.last()?;
                Some(MessageContent::Media(MediaMetadata {
                    kind: MediaType::Image,
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
                }))
            }

            MediaKind::Video(video) => Some(MessageContent::Media(MediaMetadata {
                kind: MediaType::Video,
                file: FileMetadata {
                    file_name: video.video.file_name.clone().unwrap_or("video.mp4".into()),
                    mime_type: video.video.mime_type
                        .as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "video/mp4".into()),
                    url: format!("https://api.telegram.org/file/bot<your-token>/{}", video.video.file.id),
                    size_bytes: Some(video.video.file.size as u64),
                },
            })),

            MediaKind::Voice(voice) => Some(MessageContent::Media(MediaMetadata {
                kind: MediaType::Audio,
                file: FileMetadata {
                    file_name: "voice.ogg".into(),
                    mime_type: "audio/ogg".into(),
                    url: format!("https://api.telegram.org/file/bot<your-token>/{}", voice.voice.file.id),
                    size_bytes: Some(voice.voice.file.size as u64),
                },
            })),

            MediaKind::Audio(audio) => Some(MessageContent::Media(MediaMetadata {
                kind: MediaType::Audio,
                file: FileMetadata {
                    file_name: audio.audio.title.clone().unwrap_or("audio.mp3".into()),
                    mime_type: audio.audio.mime_type.as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "video/mp4".into()),
                    url: format!("https://api.telegram.org/file/bot<your-token>/{}", audio.audio.file.id),
                    size_bytes: Some(audio.audio.file.size as u64),
                },
            })),

            MediaKind::Document(doc) => Some(MessageContent::File(FileMetadata {
                file_name: doc.document.file_name.clone().unwrap_or("file".into()),
                mime_type: doc.document.mime_type.as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "video/mp4".into()),
                url: format!("https://api.telegram.org/file/bot<your-token>/{}", doc.document.file.id),
                size_bytes: Some(doc.document.file.size as u64),
            })),

            MediaKind::Sticker(sticker) => Some(MessageContent::Media(MediaMetadata {
                kind: MediaType::Image,
                file: FileMetadata {
                    file_name: "sticker.webp".into(),
                    mime_type: "image/webp".into(),
                    url: format!("https://api.telegram.org/file/bot<your-token>/{}", sticker.sticker.file.id),
                    size_bytes: Some(sticker.sticker.file.size as u64),
                },
            })),

            MediaKind::VideoNote(video_note) => Some(MessageContent::Media(MediaMetadata {
                kind: MediaType::Video,
                file: FileMetadata {
                    file_name: "video_note.mp4".into(),
                    mime_type: "video/mp4".into(),
                    url: format!("https://api.telegram.org/file/bot<your-token>/{}", video_note.video_note.file.id),
                    size_bytes: Some(video_note.video_note.file.size as u64),
                },
            })),

            MediaKind::Animation(anim) => Some(MessageContent::Media(MediaMetadata {
                kind: MediaType::Video,
                file: FileMetadata {
                    file_name: anim.animation.file_name.clone().unwrap_or("animation.mp4".into()),
                    mime_type: anim.animation.mime_type.as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "video/mp4".into()),
                    url: format!("https://api.telegram.org/file/bot<your-token>/{}", anim.animation.file.id),
                    size_bytes: Some(anim.animation.file.size as u64),
                },
            })),

            MediaKind::Contact(contact) => Some(MessageContent::Event(Event {
                event_type: "ContactShared".into(),
                event_payload: Some(serde_json::json!({
                    "name": format!("{} {}", contact.contact.first_name, contact.contact.last_name.clone().unwrap_or_default()),
                    "phone_number": contact.contact.phone_number
                })),
            })),

            MediaKind::Location(loc) => Some(MessageContent::Event(Event {
                event_type: "LocationShared".into(),
                event_payload: Some(serde_json::json!({
                    "latitude": loc.location.latitude,
                    "longitude": loc.location.longitude
                })),
            })),

            MediaKind::Venue(venue) => Some(MessageContent::Event(Event {
                event_type: "VenueShared".into(),
                event_payload: Some(serde_json::json!({
                    "title": venue.venue.title,
                    "address": venue.venue.address,
                    "location": {
                        "lat": venue.venue.location.latitude,
                        "lon": venue.venue.location.longitude,
                    }
                })),
            })),
            
            _ => None,
        }
    } else {
        None
    }
}

/// Our plugin struct holds just the minimal shared state.
pub struct TelegramPlugin {
    /// Incoming queue for async receive_message
    incoming_tx: UnboundedSender<ChannelMessage>,
    incoming_rx: UnboundedReceiver<ChannelMessage>,
    state:   ChannelState,
    config:  DashMap<String,String>,
    secrets: DashMap<String,String>,
    bot:     Option<Bot>,
    logger: Option<PluginLogger>,
}

impl Default for TelegramPlugin {
    fn default() -> Self {
        let (tx, rx) = unbounded_channel();
        TelegramPlugin { 
            incoming_tx: tx,
            incoming_rx: rx,
            state: ChannelState::Stopped, 
            config: DashMap::new(), 
            secrets: DashMap::new(),
            bot:None, 
            logger: None}
    }
}

/// We assume that the telegram bot token is set via a secret called "telegram_token"
/// We also assume that when sending a message the participant id is the same as the chat_id.
impl TelegramPlugin {
    /// Spawn a background dispatcher if not already running.
    async fn init_dispatcher(&mut self) {
        static STARTED: OnceCell<()> = OnceCell::new();
        if STARTED.set(()).is_ok() {
            // 1) Grab the token & build the Bot
            let token = match self.secrets.get("TELEGRAM_TOKEN") {
                Some(entry) => entry.value().clone(),
                None         => String::new(),
            };
            let bot = Bot::new(token);
            self.bot = Some(bot.clone());

            // 2) Clone our plugin‐inbound channel & logger
            let tx = self.incoming_tx.clone();
            let log = self.logger.clone().unwrap();

            // 3) Build a dptree handler that fires on Message updates
            let handler = Update::filter_message()
                .endpoint(move |bot: Bot, msg: Message| {
                    let tx = tx.clone();
                    let log = log.clone();
                    async move {
                        if let Some(content) = extract_content(bot, &msg) {
                            let session = msg.chat.id.to_string();
                            let cm = ChannelMessage {
                                channel:    "telegram".into(),
                                session_id: Some(session.clone()),
                                direction:  MessageDirection::Incoming,
                                from: Participant {
                                    id:                 session.clone(),
                                    display_name:       msg.from.clone().map(|u| u.full_name()),
                                    channel_specific_id: msg.from.and_then(|u| u.username.clone()),
                                },
                                content:    Some(content),
                                id:         msg.id.to_string(),
                                timestamp:  Utc::now(),
                                to:         Vec::new(),
                                thread_id:  None,
                                reply_to_id:None,
                                metadata:   Default::default(),
                            };
                            if let Err(e) = tx.send(cm) {
                                log.log(LogLevel::Error, "telegram", &format!("queue send error: {}", e));
                            }
                        }
                        // dptree requires an Ok(()) return
                        Ok::<(), Infallible>(())
                    }
                });

            // 4) Spawn the dispatcher on the Tokio runtime
            tokio::spawn(async move {               
                Dispatcher::builder(bot, handler)
                .build()
                .dispatch()
                .await;
            });
        }
            
    }
}


async fn send_msg(msg: ChannelMessage, chat_id: String, bot: Option<Bot>, log: PluginLogger)  -> anyhow::Result<(),PluginError> {
    // 1) Extract text content
    let text = match &msg.content {
        Some(MessageContent::Text(t)) => t.clone(),
        _ => {
            log.log(LogLevel::Error, "telegram", "only Text messages supported");
            return Err(PluginError::Other("only Text messages supported".into()));
        }
    };

    // 2) Grab the Bot handle
    let bot = bot
        .as_ref()
        .ok_or_else(|| PluginError::Other("Bot not initialized".into()))?
        .clone();

    log.log(LogLevel::Debug, "telegram", "sending message…");

    // 3) Perform the async send
    let result = bot
        .send_message(chat_id.clone(), text)
        .send()             // this returns a Future
        .await
        .map_err(|e| PluginError::Other(format!("telegram send error: {}", e)))?;

    log.log(
        LogLevel::Info,
        "telegram",
        &format!("message sent to {}: message_id={}", chat_id, result.id),
    );

    Ok(())
}
#[async_trait]
impl ChannelPlugin for TelegramPlugin {
    fn name(&self) -> String {
        "telegram".to_string()
    }

    fn set_logger(&mut self, logger: PluginLogger) {
        self.logger = Some(logger);
    }

    fn get_logger(&self) -> Option<PluginLogger> {
        self.logger
    }
    
    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            name:                    "telegram".into(),
            supports_sending:        true,
            supports_receiving:      true,
            supports_text:           true,
            supports_files:          true,
            supports_media:          true,
            supports_events:         true,
            supports_typing:         true,
            supports_threading:      false,
            supports_reactions:      false,
            supports_call:           false,
            supports_buttons:        false,
            supports_links:          true,
            supports_custom_payloads:false,
            supported_events:        Vec::new(),
        }
    }

    fn set_config(&mut self, config: DashMap<String, String>) { 
        self.config = config; 
    }

    fn list_config(&self) -> Vec<String> {
        Vec::new()
    }  

    fn set_secrets(&mut self, secrets: DashMap<String, String>) { 
        self.secrets = secrets; 
    }

    fn list_secrets(&self) -> Vec<String> {
        vec!["TELEGRAM_TOKEN".to_string()]
    }

    fn state(&self) -> ChannelState {
        self.state.clone()
    }

    async fn start(&mut self) -> Result<(),PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "telegram", "start called");
        }
        self.init_dispatcher().await;
        self.state = ChannelState::Running;
        Ok(())
    }

    fn drain(&mut self) -> Result<(),PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "telegram", "drain called");
        }
        self.state = ChannelState::Draining;
        Ok(())
    }

    async fn wait_until_drained(&mut self, _timeout_ms: u64)  -> Result<(),PluginError> {
        // Since we use an unbounded channel for incoming messages, and
        // our send_message never blocks, there's nothing to wait for on drain.
        // If you had an outbound queue, you'd await that here.
        Ok(())
    }

    async fn stop(&mut self)  -> Result<(),PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "telegram", "stop called");
        }
        self.state = ChannelState::Stopped;
        Ok(())
    }
    
    async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(), PluginError> {
        let log = self
            .logger
            .as_ref()
            .ok_or_else(|| PluginError::InvalidState)?;

        log.log(LogLevel::Info, "telegram", "send a message");
        // 1) pull off the Vec<Participant> and the Option<String> by cloning them:
        let to_list = msg.to.clone();

        // 2) now you can still use `msg` freely; whenever you call send_msg, clone `msg`:
        if to_list.is_empty() {
            let error = "sending to empty participant is not possible";
            log.log(
                    LogLevel::Error,
                    "telegram",
                    error,
                );

            return Err(PluginError::Other(error.to_string()));
        } else {
            for participant in to_list {
                // clone the chat_id string
                let chat_id = participant.id.clone();

                send_msg(msg.clone(), chat_id.clone(), self.bot.clone(), log.clone())
                    .await
                    .map_err(|e| {
                        log.log(
                            LogLevel::Error,
                            "telegram",
                            &format!("failed to send to {}: {}", chat_id, e),
                        );
                        e
                    })?;
            }
        }
        Ok(())
    }
    
    async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage,PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "telegram", "receive message called");
        }
        self.incoming_rx
            .recv()
            .await
            .ok_or_else(|| PluginError::Other("receive_message channel closed".into()))
    }
}

// export all the FFI for us

export_plugin!(TelegramPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use channel_plugin::message::{ChannelMessage, MessageContent, Participant, MessageDirection};
    use channel_plugin::plugin::{ChannelState, PluginLogger, LogLevel};
    use chrono::Utc;
    use dashmap::DashMap;

    extern "C" fn test_log_fn(
        _ctx: *mut std::ffi::c_void,
        level: LogLevel,
        tag: *const i8,
        msg: *const i8,
    ) {
        // Convert C strings to Rust &str
        let tag = unsafe {
            CStr::from_ptr(tag)
                .to_str()
                .unwrap_or("<invalid tag>")
        };
        let msg = unsafe {
            CStr::from_ptr(msg)
                .to_str()
                .unwrap_or("<invalid msg>")
        };

        // If you want to inspect ctx, you can cast it back:
        // let ctx_str = unsafe {
        //     *(ctx as *const &str)
        // };
        // println!("[{}] {}: {} (ctx={})", level as u8, tag, msg, ctx_str);

        // For now, just print level, tag and message:
        println!("[{:?}] {}: {}", level, tag, msg);
    }

    #[tokio::test]
    async fn test_state_transitions_async() {
        let mut p = TelegramPlugin::default();
        p.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn });

        assert_eq!(p.state(), ChannelState::Stopped);

        p.start().await.expect("start failed");
        assert_eq!(p.state(), ChannelState::Running);

        p.drain().expect("drain failed");
        assert_eq!(p.state(), ChannelState::Draining);

        p.stop().await.expect("stop failed");
        assert_eq!(p.state(), ChannelState::Stopped);
    }

    #[tokio::test]
    async fn test_capabilities() {
        let mut p = TelegramPlugin::default();
        p.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn });
        let caps = p.capabilities();
        assert_eq!(caps.name, "telegram");
        assert!(caps.supports_text);
        assert!(caps.supports_media);
        assert!(!caps.supports_call);
    }

    #[tokio::test]
    async fn test_send_without_content_errors_async() {
        let mut p = TelegramPlugin::default();
        p.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn });
        p.start().await.expect("start");

        // default msg has no content
        let mut msg = ChannelMessage::default();
        msg.to = vec![ Participant { id:"123".into(), display_name:None, channel_specific_id:None } ];

        p.send_message(msg).await.expect_err("should error without text");
    }

    #[tokio::test]
    async fn test_send_and_receive_roundtrip_async() {
        let mut p = TelegramPlugin::default();
        p.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn });
        p.set_secrets({
            let m = DashMap::new();
            m.insert("TELEGRAM_TOKEN".into(), "fake".into());
            m
        });
        p.start().await.expect("start");

        // Simulate an incoming Telegram message
        let incoming = ChannelMessage {
            id: "in1".into(),
            session_id: Some("chat42".into()),
            direction: MessageDirection::Incoming,
            timestamp: Utc::now(),
            channel: "telegram".into(),
            from: Participant { id:"chat42".into(), display_name:None, channel_specific_id:None },
            to: vec![],
            content: Some(MessageContent::Text("hello".into())),
            thread_id: None,
            reply_to_id: None,
            metadata: Default::default(),
        };

        // Manually push into the plugin's incoming channel
        let _ = p.incoming_tx.send(incoming.clone());

        // receive it
        let got = p.receive_message().await.expect("receive");
        assert_eq!(got.id, "in1");
        assert_eq!(got.content, Some(MessageContent::Text("hello".into())));

        // Test send_message paths (won't actually call Telegram)
        let outgoing = ChannelMessage {
            id: "out1".into(),
            session_id: Some("chat42".into()),
            direction: MessageDirection::Outgoing,
            timestamp: Utc::now(),
            channel: "telegram".into(),
            from: Participant { id:"bot".into(), display_name:None, channel_specific_id:None },
            to: vec![ Participant { id:"chat42".into(), display_name:None, channel_specific_id:None } ],
            content: Some(MessageContent::Text("reply".into())),
            thread_id: None,
            reply_to_id: None,
            metadata: Default::default(),
        };

        let err = p.send_message(outgoing).await.expect_err("send_message should fail fast on bad token");
        assert!(
            format!("{}", err).contains("telegram send error"),
            "unexpected error: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_wait_until_drained_async() {
        let mut p = TelegramPlugin::default();
        p.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn });
        p.start().await.expect("start");
        p.drain().expect("drain");

        // Since we have no backlog, this should return immediately
        p.wait_until_drained(10).await.expect("drained without backlog");
    }

    #[tokio::test]
    async fn test_set_config_and_secrets_async() {
        let mut p = TelegramPlugin::default();
        p.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn });
        {
            let cfg = DashMap::new();
            cfg.insert("foo".into(), "bar".into());
            p.set_config(cfg.clone());
            let entry = p.config.get("foo").expect("`foo` must exist");
            assert_eq!(entry.value(), "bar");
        }

        let sec = DashMap::new();
        sec.insert("TELEGRAM_TOKEN".into(), "token".into());
        p.set_secrets(sec.clone());
        {
            let entry = p.secrets.get("TELEGRAM_TOKEN").expect("telegra token not set");
            assert_eq!(entry.value(), "token");
        }
    }
}