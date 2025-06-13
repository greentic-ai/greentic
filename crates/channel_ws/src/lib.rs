// src/ws_plugin.rs

use std::net::SocketAddr;
use dashmap::DashMap;
use async_trait::async_trait;
use tokio::{net::{TcpListener, TcpStream}, sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender}};
use tokio_tungstenite::{accept_async, tungstenite::Message as WsMsg};
use futures_util::{StreamExt, SinkExt};
use channel_plugin::{
    export_plugin,
    message::{ChannelCapabilities, ChannelMessage, MessageContent, Participant, MessageDirection},
    plugin::{ChannelPlugin, ChannelState, PluginError, PluginLogger, LogLevel},
};
use uuid::Uuid;
use chrono::Utc;

/// Commands sent to the manager task
enum Command {
    Register {
        session: String,
        sender: UnboundedSender<WsMsg>,
    },
    Unregister {
        session: String,
    },
    SendMessage {
        session: String,
        msg: WsMsg,
    },
}

/// A lock‐free async WebSocket server plugin (plain WS).
pub struct WsPlugin {
    cmd_tx: Option<UnboundedSender<Command>>,
    incoming_tx:  UnboundedSender<ChannelMessage>,
    msg_rx: UnboundedReceiver<ChannelMessage>,
    logger: Option<PluginLogger>,
    state:  ChannelState,
    addr:   String,
}

impl Default for WsPlugin {
    fn default() -> Self {
        let (incoming_tx, incoming_rx) = unbounded_channel();
        let addr = "0.0.0.0:8888".to_string();

        WsPlugin {     
            cmd_tx: None,
            incoming_tx,
            msg_rx: incoming_rx,
            logger: None, 
            state: ChannelState::Stopped, 
            addr 
        }
    }
}

impl WsPlugin {

    pub fn address(&self) -> String {
        self.addr.clone()
    }
    /// manager task: runs single-threaded, no locks needed for map
    async fn manager_loop(
        mut cmd_rx: UnboundedReceiver<Command>,
    ) {
        let map: DashMap<String, UnboundedSender<WsMsg>> = DashMap::new();
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::Register { session, sender } => {
                    map.insert(session, sender);
                }
                Command::Unregister { session } => {
                    map.remove(&session);
                }
                Command::SendMessage { session, msg } => {
                    if let Some(tx) = map.get(&session) {
                        let _ = tx.send(msg);
                    }
                }
            }
        }
    }

    /// Spawn a handler for each connection
    fn spawn_connection(
        cmd_tx: UnboundedSender<Command>,
        msg_tx: UnboundedSender<ChannelMessage>,
        stream: TcpStream,
        peer: SocketAddr,
        log: PluginLogger,
    ) {
        tokio::spawn(async move {
            let session = peer.to_string();
            match accept_async(stream).await {
                    Ok(ws_stream) => {
                        println!("@@@ GOT HERE 7");
                    let (mut write, mut read) = ws_stream.split();
                    // channel for outbound frames
                    let (tx_out, mut rx_out) = unbounded_channel();
                    // register
                    let _ = cmd_tx.send(Command::Register { session: session.clone(), sender: tx_out });
                    loop {
                        tokio::select! {
                            // inbound
                            msg = read.next() => match msg {
                                Some(Ok(WsMsg::Text(txt))) => {
                                    println!("@@@ GOT HERE 8");
                                    let _ = write.send(WsMsg::Text(txt.clone())).await;

                                    log.log(LogLevel::Info, "ws", &format!("recv {}: {}", session, txt));
                                    
                                    let _ = msg_tx.send(ChannelMessage{
                                        channel:"ws".into(),
                                        session_id:Some(session.clone()),
                                        direction:MessageDirection::Incoming,
                                        from:Participant{id:session.clone(),display_name:None,channel_specific_id:None},
                                        content:Some(MessageContent::Text(txt.to_string())),
                                        id:Uuid::new_v4().to_string(),
                                        timestamp:Utc::now(),
                                        to:Vec::new(),thread_id:None,reply_to_id:None,metadata:DashMap::new(),
                                    });
                                }
                                _ => break,
                            },
                            // outbound
                            frame = rx_out.recv() => match frame {
                                Some(frame) => { 
                                    println!("@@@ GOT HERE 9");
                                    let _ = write.send(frame).await; 
                                }
                                None => break,
                            }
                        }
                    }
                    let _ = cmd_tx.send(Command::Unregister { session });
                }
                Err(e) => {
                    log.log(LogLevel::Error, "ws", &format!("WebSocket handshake failed: {}", e));
                }
            }
        });
    }
}

#[async_trait]
impl ChannelPlugin for WsPlugin {
    fn name(&self) -> String { "ws".into() }
    fn set_logger(&mut self, lg: PluginLogger) { self.logger = Some(lg); }
    fn get_logger(&self) -> Option<PluginLogger> { self.logger  }
    fn set_config(&mut self, cfg: DashMap<String,String>) { 
        let address: String = cfg
            .get("address")                          // Option<Ref<_, String, String>>
            .map(|e| e.value().clone())              // Option<String>
            .unwrap_or_else(|| "0.0.0.0".to_string());

        // port as a String
        let port: String = cfg
            .get("port")
            .map(|e| e.value().clone())
            .unwrap_or_else(|| "8888".to_string());
        let addr = format!("{}:{}", address, port);
        self.addr = addr;
    }
    fn list_config(&self) -> Vec<String> { vec!["address".into(),"port".into()] }
    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities { name:"ws".into(),supports_sending:true,supports_receiving:true,supports_text:true,..Default::default() }
    }
    async fn start(&mut self) -> Result<(),PluginError> {
        // Use the current runtime’s handle:
        let (cmd_tx, cmd_rx) = unbounded_channel();
        println!("@@@ GOT HERE 1");
        self.cmd_tx = Some(cmd_tx.clone());
        tokio::spawn(WsPlugin::manager_loop(cmd_rx));
        println!("@@@ GOT HERE 2");
        let log =self.logger.clone().expect("Logger was not passed to channel ws");
        let incoming_tx = self.incoming_tx.clone();
        // spawn accept loop on Tokio runtime
        let addr = self.addr.clone();
        println!("@@@ GOT HERE 3");
        //tokio::spawn(async move {
            let listener = TcpListener::bind(addr).await.unwrap();
            println!("@@@ GOT HERE 4");
            while let Ok((stream, peer)) = listener.accept().await {
                println!("@@@ GOT HERE 5");
                WsPlugin::spawn_connection(
                    cmd_tx.clone(),
                    incoming_tx.clone(), // pass the sender
                    stream,
                    peer,
                    log,
                );
            }
       // });
        self.state = ChannelState::Running;
        Ok(())
    }
    async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(),PluginError> {
        println!("@@@ REMOVE: SEND MESSAGE");
        if let Some(txt) = msg.content.as_ref().and_then(|c| match c { MessageContent::Text(t) => Some(t.clone()), _=>None }) {
            let _ = self.cmd_tx.clone().expect("send called before start").send(Command::SendMessage{ session: msg.session_id.clone().unwrap(), msg: WsMsg::Text(txt.into()) });
        }
        Ok(())
    }
    async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage,PluginError> {
        println!("@@@ REMOVE: SEND MESSAGE");
        self.msg_rx.recv().await.ok_or_else(||PluginError::Other("receive closed".into()))
    }
    fn drain(&mut self)->Result<(),PluginError>{ Ok(()) }
    async fn wait_until_drained(&mut self,_u:u64)->Result<(),PluginError>{Ok(())}
    async fn stop(&mut self)->Result<(),PluginError>{ Ok(()) }
    fn state(&self)->ChannelState{ChannelState::Running}
    fn set_secrets(&mut self,_s:DashMap<String,String>){ }
    fn list_secrets(&self)->Vec<String>{ vec![] }
}

export_plugin!(WsPlugin);



#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use std::net::TcpListener;
    use dashmap::DashMap;
    use std::thread::sleep;
    use std::time::Duration;
    use channel_plugin::plugin::PluginLogger;
    use channel_plugin::message::{ChannelMessage, MessageContent, Participant, MessageDirection};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::{tungstenite::Message as WsMsg};

    impl WsPlugin {
        /// Test‐only: push a fake incoming message directly into the queue.
        pub fn inject(&self, msg: ChannelMessage) {
            let _ = self.incoming_tx.send(msg);
        }
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

    #[tokio::test]
    async fn test_inject_and_poll() {
        let mut plugin = WsPlugin::default();
        let cm = ChannelMessage {
            channel:    "ws".into(),
            session_id: Some("test-session".into()),
            direction:  MessageDirection::Incoming,
            from:       Participant { id: "user1".into(), display_name: None, channel_specific_id: None },
            content:    Some(MessageContent::Text("hello".into())),
            id:         "msg1".into(),
            timestamp:  Utc::now(),
            to:         vec![],
            thread_id:  None,
            reply_to_id:None,
            metadata:   DashMap::new(),
        };
        plugin.inject(cm.clone());
        let polled = plugin.receive_message().await.expect("poll should return message");
        assert_eq!(polled.id, cm.id);
        assert_eq!(polled.content, cm.content);
    }

    #[tokio::test]
    async fn test_plain_ws_roundtrip() {
        // 1) Bind a TCP listener on an ephemeral port
        let listener = TcpListener::bind("127.0.0.1:0")
            .expect("bind");
        let port = listener.local_addr().unwrap().port();

        // 2) Configure and start our plugin
        let mut plugin = WsPlugin::default();
        let cfg = DashMap::new();
        cfg.insert("address".into(), "127.0.0.1".into());
        //cfg.insert("port".into(), port.to_string());
        cfg.insert("port".into(),"8888".to_string());
        plugin.set_config(cfg);
        let logger = PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn };
        plugin.set_logger(logger);

        plugin.start().await.expect("start failed");

        // Give the server a moment to bind and begin listening
        sleep(Duration::from_millis(200000));

        // 3) Connect a test client
        let url = format!("ws://127.0.0.1:{}", port);
        let (mut socket, _) = connect_async(&url).await.expect("Can't connect");

        // 4) Send a ping frame
        socket.send(WsMsg::Text("ping".into())).await.unwrap();

        // 5) The server echoes back
        let msg = socket.next()
            .await
            .expect("expected a frame")
            .expect("frame error");
        assert_eq!(msg, WsMsg::Text("ping".into()));

        // 6) Plugin should have received it
        let cm = plugin.receive_message().await.expect("receive_message");
        assert_eq!(
            cm.content,
            Some(MessageContent::Text("ping".into()))
        );

        // 7) Now send from plugin -> client
        plugin.send_message(cm.clone())
            .await
            .expect("send_message");

        // 8) Client sees it again
        let msg2 = socket.next()
            .await
            .expect("expected a frame")
            .expect("frame error");
        assert_eq!(msg2, WsMsg::Text("ping".into()));

        
    }
}