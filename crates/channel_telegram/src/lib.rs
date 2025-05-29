// channel_telegram/src/lib.rs

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex,},
    time::{Duration, Instant},
};

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
use tokio::{runtime::Handle, task};



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
    state:   ChannelState,
    config:  HashMap<String,String>,
    secrets: HashMap<String,String>,
    queue:   Arc<(Mutex<VecDeque<ChannelMessage>>, Condvar)>,
    bot:     Option<Bot>,
    logger: Option<Arc<PluginLogger>>,
}

impl Default for TelegramPlugin {
    fn default() -> Self {
        // each plugin gets a brand-new queue
        let queue = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        TelegramPlugin { state: ChannelState::Stopped, config: HashMap::new(), secrets: HashMap::new(),queue, bot:None, logger: None}
    }
}

/// We assume that the telegram bot token is set via a secret called "telegram_token"
/// We also assume that when sending a message the participant id is the same as the chat_id.
impl TelegramPlugin {
    /// Spawn a background dispatcher if not already running.
    fn init_dispatcher(&mut self) {
        static STARTED: OnceCell<()> = OnceCell::new();
        if STARTED.set(()).is_ok() {
            // ensure queue exists
            let q = Arc::clone(&self.queue);
            let token = self.secrets.get("TELEGRAM_TOKEN").cloned().unwrap_or_default();
            let bot = Bot::new(
                token
            );
            self.bot = Some(bot.clone());
            let bot_clone = bot.clone();
            let logger_clone = self.logger.as_ref().unwrap().clone();
            let queue_clone = Arc::clone(&self.queue);

            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::spawn(async move {
                    Dispatcher::builder(bot_clone, Update::filter_message().endpoint(
                        move | bot: Bot, msg: Message| {
                            let chat_id = msg.chat.id;
                            let session_id = format!("{}",chat_id);
                            // this closure now only does an Arc::clone and calls `handle_update`
                            handle_update(session_id, bot, msg, queue_clone.clone(), logger_clone.clone())
                        }
                    ))
                    .build()
                    .dispatch()
                    .await;
                });
            } else {
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async move {
                        Dispatcher::builder(bot_clone, Update::filter_message().endpoint(
                            move |bot: Bot, msg: Message| {
                                let chat_id = msg.chat.id;
                                let session_id = format!("{}",chat_id);
                                handle_update(session_id, bot, msg, queue_clone.clone(), logger_clone.clone())
                            }
                        ))
                        .build()
                        .dispatch()
                        .await;
                    });
                });
            }

            self.queue = q;
        }
    }
}

async fn handle_update(
    session_id: String,
    bot: Bot,
    msg: TelegramMessage,
    queue:   Arc<(Mutex<VecDeque<ChannelMessage>>, Condvar)>,
    logger: Arc<PluginLogger>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        logger.log(LogLevel::Debug, "telegram", "extracting content");
                                
        if let Some(content) = extract_content(bot,&msg) {
            let abstract_msg = ChannelMessage {
                id:        msg.id.to_string(),
                session_id: Some(session_id),
                direction: MessageDirection::Incoming,
                timestamp: Utc::now(),
                channel:   "telegram".into(),
                from: Participant {
                    id:                 msg.from.as_ref().map(|u| u.id.to_string()).unwrap_or_default(),
                    display_name:       msg.from.as_ref().map(|u| u.full_name()),
                    channel_specific_id: msg.from.as_ref().and_then(|u| u.username.clone()),
                },
                to:           vec![],
                content:      Some(content),
                thread_id:    None,
                reply_to_id:  None,
                metadata:     Default::default(),
            };
            
            let (lock, cvar) = &*queue;
            let mut guard = lock.lock().unwrap();
            guard.push_back(abstract_msg);
            cvar.notify_one();
            
        }
    Ok(())
}

fn send_msg(msg: ChannelMessage, chat_id: String, bot: Option<Bot>)  -> anyhow::Result<(),PluginError> {

    // 3) Extract text content
    let text = match &msg.content {
        Some(MessageContent::Text(t)) => t.clone(),
        _ => return Err(PluginError::Other("only Text messages supported".into())),
    };

    // 4) Grab the Bot
    let bot = bot
        .clone()
        .ok_or_else(|| PluginError::Other("Bot not initialized".into()))?
        .clone();

    // 5) Perform the async send under a runtime
    let req = bot.send_message(chat_id, text);

    // Now run that Future to completion in whichever runtime we have:
    let send_fut = req.send();
    let res = if Handle::try_current().is_ok() {
        // We're inside Tokio already, so block in place rather than spawn a new runtime
        task::block_in_place(|| {
            Handle::current().block_on(send_fut)
        })
    } else {
        // No runtime, so spin one up
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| PluginError::Other(format!("failed to start runtime: {}", e)))?;
        rt.block_on(send_fut)
    };

    // 6) Map errors into PluginError
    res.map_err(|e| PluginError::Other(format!("telegram send error: {}", e)))?;

    Ok(())
}

impl ChannelPlugin for TelegramPlugin {
    fn name(&self) -> String {
        "telegram".to_string()
    }

    fn set_logger(&mut self, logger: PluginLogger) {
        self.logger = Some(Arc::new(logger));
    }
    

    fn send(&mut self, msg: ChannelMessage) -> anyhow::Result<(),PluginError> {
        if self.state != ChannelState::Running {
            return Err(PluginError::InvalidState);
        }

        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "telegram", "send a message");
        }
        // 1) pull off the Vec<Participant> and the Option<String> by cloning them:
        let to_list = msg.to.clone();
        let session_id_opt = msg.session_id.clone();

        // 2) now you can still use `msg` freely; whenever you call send_msg, clone `msg`:
        if to_list.is_empty() {
            if let Some(session_id) = session_id_opt {
                return send_msg(msg.clone(), session_id, self.bot.clone());
            } else {
                let error = "sending to empty participant or no session_id is not possible";
                if let Some(log) = &self.logger {
                    log.log(
                        LogLevel::Error,
                        "telegram",
                        error,
                    );
                }
                return Err(PluginError::Other(error.to_string()));
            }
        } else {
            for participant in to_list {
                // clone the chat_id string
                let chat_id = participant.id.clone();

                if chat_id.is_empty() {
                    // fallback to session_id if participant is empty
                    if let Some(session_id) = session_id_opt.clone() {
                        return send_msg(msg.clone(), session_id, self.bot.clone());
                    } else {
                        let error = "sending to empty participant or no session_id is not possible";
                        if let Some(log) = &self.logger {
                            log.log(
                                LogLevel::Error,
                                "telegram",
                                error,
                            );
                        }
                        return Err(PluginError::Other(error.to_string()));
                    }
                } else {
                    // normal case
                    return send_msg(msg.clone(), chat_id, self.bot.clone());
                }
            }
        }
        Ok(())
    }

    fn poll(&self) -> Result<ChannelMessage, PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "telegram", "got a message poll");
        }

        let (lock, cvar) = &*self.queue;
        // lock + wait until queue is non-empty or state changes
        let mut guard = lock.lock().unwrap();
        guard = cvar
            .wait_while(guard, |q| q.is_empty())// && self.state == ChannelState::Running)
            .unwrap();

        // pop or error
        guard
            .pop_front()
            .ok_or_else(|| PluginError::Other("channel stopped or drained".into()))

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

    fn set_config(&mut self, config: std::collections::HashMap<String, String>) { 
        self.config = config; 
    }

    fn list_config(&self) -> Vec<String> {
        Vec::new()
    }  

    fn set_secrets(&mut self, secrets: std::collections::HashMap<String, String>) { 
        self.secrets = secrets; 
    }

    fn list_secrets(&self) -> Vec<String> {
        vec!["TELEGRAM_TOKEN".to_string()]
    }

    fn state(&self) -> ChannelState {
        self.state.clone()
    }

    fn start(&mut self) -> Result<(),PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "telegram", "start called");
        }
        self.init_dispatcher();
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

    fn wait_until_drained(&mut self, timeout_ms: u64)  -> Result<(),PluginError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let (lock, cvar) = &*self.queue;
        loop {
            if Instant::now() >= deadline { return Err(PluginError::Timeout(timeout_ms));}
            if self.state != ChannelState::Draining { return Ok(()); }
            let guard = lock.lock().unwrap();
            if guard.is_empty() { return Ok(()); }
            let _ = cvar.wait_timeout(guard, Duration::from_millis(50)).unwrap();
        }
    }

    fn stop(&mut self)  -> Result<(),PluginError> {
        if let Some(log) = &self.logger {
            log.log(LogLevel::Info, "telegram", "stop called");
        }
        self.state = ChannelState::Stopped;
        Ok(())
    }
}

// export all the FFI for us

export_plugin!(TelegramPlugin);


#[cfg(test)]
mod tests {
    use super::*;
    use channel_plugin::message::{ChannelMessage, MessageDirection};
    use std::collections::HashMap;
    use chrono::Utc;

    extern "C" fn test_log_fn(
        _ctx: *mut c_void, 
        level: LogLevel, 
        tag: *const i8, 
        msg: *const i8
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

    /// Helper to push a fake message into the global queue.
    fn push_msg(queue: Arc<(Mutex<VecDeque<ChannelMessage>>, Condvar)>,msg: ChannelMessage) {
        let (lock, cvar) = &*queue;
        let mut guard = lock.lock().unwrap();
        guard.push_back(msg);
        cvar.notify_one();
    }

    #[test]
    fn test_state_transitions() {
        let mut p = TelegramPlugin::default();
        let logger = PluginLogger{ ctx: std::ptr::null_mut(), log_fn: test_log_fn };
        p.set_logger(logger);
        assert_eq!(p.state(), ChannelState::Stopped);

        p.start().expect("could not start");
        assert_eq!(p.state(), ChannelState::Running);

        p.drain().expect("could not drain");
        assert_eq!(p.state(), ChannelState::Draining);

        p.stop().expect("could not stop");
        assert_eq!(p.state(), ChannelState::Stopped);
    }

    #[test]
    fn test_capabilities_fields() {
        let mut p = TelegramPlugin::default();
        let logger = PluginLogger{ ctx: std::ptr::null_mut(), log_fn: test_log_fn };
        p.set_logger(logger);
        let caps = p.capabilities();
        assert_eq!(&caps.name, "telegram");
        assert!(caps.supports_text);
        assert!(caps.supports_media);
        assert!(!caps.supports_call);
    }

    #[test]
    fn test_send_without_content_errors() {
        let mut p = TelegramPlugin::default();
        let logger = PluginLogger{ ctx: std::ptr::null_mut(), log_fn: test_log_fn };
        p.set_logger(logger);
        p.start().expect("could not start");

        // default ChannelMessage has content = None
        let mut msg = ChannelMessage::default();
        msg.to = vec![Participant{ 
            id: "test".to_string(), 
            display_name: None, 
            channel_specific_id: None }
        ];
        p.send(msg).expect_err("did not err");

    }

   #[test]
    fn test_poll_blocks_until_message() {
        let mut p = TelegramPlugin::default();
        let logger = PluginLogger{ ctx: std::ptr::null_mut(), log_fn: test_log_fn };
        p.set_logger(logger);
        p.start().expect("could not start");

        // spawn a helper thread which will push a message after a brief delay
        let queue = p.queue.clone();
        let push_handle = std::thread::spawn(move || {
            // give poll() a moment to go to sleep on the condvar
            std::thread::sleep(std::time::Duration::from_millis(50));
            let mut fake = ChannelMessage::default();
            fake.id = "xyz".into();
            fake.direction = MessageDirection::Incoming;
            fake.timestamp = Utc::now();
            fake.channel = "Telegram".into();
            push_msg(queue,fake);
        });

        // this will block until the helper thread pushes the message
        let got = p.poll().expect("poll should return once a message is available");
        assert_eq!(got.id, "xyz");

        // make sure our helper thread has finished
        push_handle.join().unwrap();
    }

    #[test]
    fn test_wait_until_drained_timeout() {
        let mut p = TelegramPlugin::default();
        let logger = PluginLogger{ ctx: std::ptr::null_mut(), log_fn: test_log_fn };
        p.set_logger(logger);
        p.start().expect("could not start");
        p.drain().expect("could not drain");

        // Ensure there's something in the queue so it cannot drain immediately
        let fake = ChannelMessage {
            id: "wait".into(), .. ChannelMessage::default() };
        push_msg(p.queue.clone(),fake);

        // Should time out (we passed zero ms)
        p.wait_until_drained(0).expect_err("could not drain");
        // After timeout, state is still Draining
        assert_eq!(p.state(), ChannelState::Draining);
    }

    #[test]
    fn test_set_config_and_secrets() {
        let mut p = TelegramPlugin::default();
        let logger = PluginLogger{ ctx: std::ptr::null_mut(), log_fn: test_log_fn };
        p.set_logger(logger);
        let mut cfg = HashMap::new();
        cfg.insert("telegram_chat_id".into(), "123".into());
        p.set_config(cfg.clone());
        assert_eq!(p.config.get("telegram_chat_id"), Some(&"123".into()));

        let mut sec = HashMap::new();
        sec.insert("TELEGRAM_TOKEN".into(), "tok".into());
        p.set_secrets(sec.clone());
        assert_eq!(p.secrets.get("TELEGRAM_TOKEN"), Some(&"tok".into()));
    }
}
