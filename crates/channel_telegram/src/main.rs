// main.rs
// An example main program that uses the TelegramPlugin to echo incoming messages.

use std::ffi::{c_void, CStr};
use std::path::Path;
use channel_plugin::fakse_session::{fake_get_or_create_session, fake_get_session, fake_invalidate_session};
use channel_telegram::{greentic_register_session_fns, TelegramPlugin};
use chrono::Utc;
use channel_plugin::plugin::{run_blocking, ChannelPlugin, LogLevel, PluginLogger};
use channel_plugin::message::{ChannelMessage, MessageContent, MessageDirection, Participant};
use dotenvy::from_path;
use dashmap::DashMap;



fn main() {

    greentic_register_session_fns(
        fake_get_session,
        fake_get_or_create_session,
        fake_invalidate_session,
    );
    
    let env_path = Path::new("./greentic/secrets/.env");
    from_path(env_path).expect("failed to read ./greentic/secrets/.env");
    // Create the plugin and set your Telegram bot token
    let mut plugin = TelegramPlugin::default();
    let logger = PluginLogger{ ctx: std::ptr::null_mut(), log_fn: test_log_fn };
    plugin.set_logger(logger, LogLevel::Debug);
    let secrets = DashMap::new();
    secrets.insert("TELEGRAM_TOKEN".to_string(), std::env::var("TELEGRAM_TOKEN").expect("TELEGRAM_TOKEN was not set"));
    plugin.set_secrets(secrets);
    let _ = run_blocking(async move {
        // Start the plugin (spawns the dispatcher)
        plugin.start().await.map_err(|e| anyhow::anyhow!("Failed to start plugin: {:?}", e)).expect("could not start plugin");
        println!("Bot started, waiting for messages...");

        // Event loop: poll for incoming messages and echo them back
        loop {

            let incoming: ChannelMessage = plugin.receive_message().await.map_err(|e| anyhow::anyhow!("Poll error: {:?}", e)).expect("could not receive message");

            // Only handle text messages
            match incoming.content.clone() {
                Some(contents) if contents.len() == 1 => {
                    if let MessageContent::Text(text) = &contents[0] {
                        println!("Received message: {}", text);

                        let chat_id = incoming.from.id;

                        // Build reply message
                        let reply_text = format!("Received: {}", text);
                        let reply = ChannelMessage {
                            session_id: None,
                            thread_id: Some(chat_id.clone()),
                            direction: MessageDirection::Outgoing,
                            timestamp: Utc::now(),
                            channel: incoming.channel.clone(),
                            content: Some(vec![MessageContent::Text(reply_text)]),
                            from: Participant { id: "channel_telegram".to_string(), display_name: Some("channel_telegra".to_string()), channel_specific_id: None },
                            to: vec![Participant { id: chat_id.clone(), display_name:  incoming.from.display_name, channel_specific_id: None }],
                            reply_to_id: Some(incoming.id.clone()),
                            metadata: Default::default(),
                            ..Default::default()
                        };

                        // Send the reply
                        plugin.send_message(reply).await.map_err(|e| anyhow::anyhow!("Send error: {:?}", e)).expect("could not send message");
                    }
                }
                _ => {}
            }
        }
    });

}

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