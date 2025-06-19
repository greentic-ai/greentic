// src/ws_plugin.rs

use std::{net::SocketAddr, thread::{sleep}, time::Duration};
use dashmap::DashMap;
use async_trait::async_trait;

use tokio::{net::{TcpListener, TcpStream}, sync::{broadcast, mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender}}, task::JoinHandle};
use tokio_tungstenite::{accept_async, tungstenite::Message as WsMsg};
use futures_util::{StreamExt, SinkExt};
use channel_plugin::{
    export_plugin,
    message::{build_user_joined_event, build_user_left_event, get_user_joined_left_events, ChannelCapabilities, ChannelMessage, MessageContent, MessageDirection, Participant, TextMessage},
    plugin::{ChannelPlugin, ChannelState, DefaultRoutingSupport, LogLevel, PluginError, PluginLogger, RoutingSupport},
};
use uuid::Uuid;
use chrono::Utc;


/// Commands sent to the manager task
enum Command {
    Register {
        peer: String,
        sender: UnboundedSender<WsMsg>,
    },
    Unregister {
        peer: String,
    },
    SendMessage {
        peer: String,
        msg: WsMsg,
    },
}

/// A lock‐free async WebSocket server plugin (plain WS).
pub struct WsPlugin {
    cmd_tx: Option<UnboundedSender<Command>>,
    incoming_tx:  UnboundedSender<ChannelMessage>,
    msg_rx: UnboundedReceiver<ChannelMessage>,
    routing: DefaultRoutingSupport,
    logger: Option<PluginLogger>,
    log_level: Option<LogLevel>,
    state:  ChannelState,
    addr:   String,
    shutdown_tx: Option<broadcast::Sender<()>>,
    manager_task: Option<JoinHandle<()>>,
    ws_task: Option<JoinHandle<()>>,
}

impl Default for WsPlugin {
    fn default() -> Self {
        let (incoming_tx, incoming_rx) = unbounded_channel();
        let addr = "0.0.0.0:8888".to_string();

        WsPlugin {     
            cmd_tx: None,
            incoming_tx,
            msg_rx: incoming_rx,
            routing: DefaultRoutingSupport::default(),
            logger: None, 
            log_level: None,
            state: ChannelState::Stopped, 
            addr,
            shutdown_tx: None,
            manager_task: None,
            ws_task: None,
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
        msg_tx: UnboundedSender<ChannelMessage>,
        name: String,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) {
        let map: DashMap<String, UnboundedSender<WsMsg>> = DashMap::new();
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    println!("@@@ SHUTTING DOWN MANAGER LOOP");
                    break;
                }
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        Command::Register { peer, sender } => {
                            map.insert(peer, sender);
                        }
                        Command::Unregister { peer } => {
                            if let Some(session_id) = SessionApi::get_session_id(&name, &peer).await {
                                let user_left_event = build_user_left_event(&name.clone(), &peer.clone(), Some(session_id.clone()));
                                let _ = msg_tx.send(user_left_event);
                                SessionApi::invalidate_session_id(&session_id).await;
                            }
                            map.remove(&peer);
                        }
                        Command::SendMessage {peer, msg } => {
                            println!("@@@ REMOVE trying to get sender");
                            if let Some(tx) = map.get(&peer) {
                                println!("@@@ REMOVE found sender");
                                let result = tx.send(msg);
                                println!("@@@ REMOVE sent: {:?}",result);
                            }
                        }
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
        name: String,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) {
        tokio::spawn(async move {
            let peer = peer.to_string();
            let name = name.clone();
            match accept_async(stream).await {
                    Ok(ws_stream) => {
                        println!("@@@ GOT HERE 7");
                    let (mut write, mut read) = ws_stream.split();
                    // channel for outbound frames
                    let (tx_out, mut rx_out) = unbounded_channel();
                    // register
                    let _ = cmd_tx.send(Command::Register { peer: peer.clone(), sender: tx_out });
                    let session_id = match SessionApi::get_session_id(&name, &peer).await {
                        Some(session) => session,
                        None => SessionApi::get_or_create_session_id(&name, &peer).await,
                    };
                    // emit UserJoined event
                    let user_joined_event = build_user_joined_event(&name.clone(), &peer.clone(), Some(session_id));
                    let _ = msg_tx.send(user_joined_event);
                    loop {
                        tokio::select! {
                            _ = shutdown_rx.recv() => {
                                println!("@@@ GOT HERE CONNECTION SHUTDOWN");
                                break;
                            }
                            // inbound
                            msg = read.next() => match msg{
                                Some(Ok(WsMsg::Text(txt))) => {
                                    println!("@@@ GOT HERE 8");
                                    let session_id = match SessionApi::get_session_id(&name, &peer).await {
                                        Some(session) => session,
                                        None => SessionApi::get_or_create_session_id(&name, &peer).await,
                                    };
                                    println!("@@@ GOT HERE 9: {:?} {:?}",session_id, txt);
                                    log.log(LogLevel::Info, &name, &format!("recv {}: {}", peer, txt));
                                    let cm = ChannelMessage{
                                        channel:name.clone().into(),
                                        node: None,
                                        flow: None,
                                        session_id: Some(session_id.clone()),
                                        direction:MessageDirection::Incoming,
                                        from:Participant{id:peer.clone(),display_name:None,channel_specific_id:None},
                                        content:Some(MessageContent::Text(txt.to_string())),
                                        id:Uuid::new_v4().to_string(),
                                        timestamp:Utc::now(),
                                        to:Vec::new(),thread_id:None,reply_to_id:None,metadata:DashMap::new(),
                                    };
                                    let _ = msg_tx.send(cm);

                                }
                                _ => break,
                            },
                            // outbound
                            frame = rx_out.recv() => match frame {
                                Some(frame) => { 
                                    println!("@@@ GOT HERE 10");
                                    let _ = write.send(frame).await; 
                                }
                                None => break,
                            }
                        }
                    }
                    let _ = cmd_tx.send(Command::Unregister { peer });
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
    fn get_routing_support(&self) -> Option<&dyn RoutingSupport> { Some(&self.routing) }
    fn set_logger(&mut self, logger: PluginLogger, log_level: LogLevel) {
        self.logger = Some(logger);
        self.log_level = Some(log_level);
    }

    fn get_logger(&self) -> Option<PluginLogger> {
        self.logger
    }

    fn get_log_level(&self) -> Option<LogLevel>{
        self.log_level
    }
    fn set_config(&mut self, cfg: DashMap<String,String>) { 
        let address: String = cfg
            .get("WS_ADDRESS")                          // Option<Ref<_, String, String>>
            .map(|e| e.value().clone())              // Option<String>
            .unwrap_or_else(|| "0.0.0.0".to_string());

        // port as a String
        let port: String = cfg
            .get("WS_PORT")
            .map(|e| e.value().clone())
            .unwrap_or_else(|| "8888".to_string());
        let addr = format!("{}:{}", address, port);
        self.addr = addr;
    }
    fn list_config(&self) -> Vec<String> { vec!["WS_ADDRESS".into(),"WS_PORT".into()] }
    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities { name:"ws".into(),supports_routing: true,supports_sending:true,supports_receiving:true,supports_text:true,supported_events:get_user_joined_left_events(),..Default::default() }
    }
    async fn start(&mut self) -> Result<(),PluginError> {
        self.info("ws: starting");
        // Use the current runtime’s handle:
        let (cmd_tx, cmd_rx) = unbounded_channel();
        println!("@@@ GOT HERE 1");
        self.cmd_tx = Some(cmd_tx.clone());
        let name = self.name().clone();
        println!("@@@ GOT HERE 2");
        let log = self.logger.clone().expect("Logger was not passed to channel ws");
        let incoming_tx = self.incoming_tx.clone();
        let (shutdown_tx, _) = broadcast::channel::<()>(16);
        self.shutdown_tx = Some(shutdown_tx.clone());
        let shutdown_rx = shutdown_tx.subscribe();
        let manager = tokio::spawn(WsPlugin::manager_loop(cmd_rx, incoming_tx.clone(), name.clone(), shutdown_rx,));
        self.manager_task = Some(manager);
        // spawn accept loop on Tokio runtime
        let addr = self.addr.clone();

        self.state = ChannelState::Running;
        println!("@@@ GOT HERE 3");

        let listener = TcpListener::bind(addr.clone()).await.unwrap();
        println!("@@@ GOT HERE 4: {}",addr);
        self.state = ChannelState::Running;
        let name = self.name();
        let task = tokio::spawn(async move {
            while let Ok((stream, peer)) = listener.accept().await {
                println!("@@@ GOT HERE 5");
                let conn_shutdown_rx = shutdown_tx.subscribe();
                WsPlugin::spawn_connection(
                    cmd_tx.clone(),
                    incoming_tx.clone(),
                    stream,
                    peer,
                    log.clone(),
                    name.clone(),
                    conn_shutdown_rx,
                );
            }
        });
        self.ws_task = Some(task);


        Ok(())
    }
    async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(),PluginError> {
        println!("@@@ REMOVE: SEND MESSAGE {:?}",msg);
        self.info("ws: send message");
        if let Some(txt) = msg.content.as_ref().and_then(|c| match c { MessageContent::Text(t) => Some(t.clone()), _=>None }) {
            println!("@@@ REMOVE: SENDING {:?}",txt);
            let result = match serde_json::from_str::<TextMessage>(&txt) 
            {
                Ok(json) => self.cmd_tx.clone().expect("send called before start").send(Command::SendMessage{ peer: msg.from.id.clone(), msg: WsMsg::Text(json.text.into()) }),
                Err(_) =>  self.cmd_tx.clone().expect("send called before start").send(Command::SendMessage{ peer: msg.from.id.clone(), msg: WsMsg::Text(txt.into()) }),
            };
            if result.is_err() {
                let error = format!("Got a send error: {:?}",result);
                println!("@@@ REMOVE ERROR: {:?}",error);
                self.error(&error);
                return Err(PluginError::Other(error));
            } else {
                println!("@@@@ REMOVE SENT");
            }
        }
        Ok(())
    }
    async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage,PluginError> {
        println!("@@@ REMOVE: GOT TO RECEIVE MESSAGE");
        match self.msg_rx.recv().await {
            Some(msg) => {
                self.info("ws: receive message");
                Ok(msg)
            }
            None => Err(PluginError::Other("receive_message channel closed".into())),
        }
    }
    fn drain(&mut self)->Result<(),PluginError>{ Ok(()) }
    async fn wait_until_drained(&mut self,_u:u64)->Result<(),PluginError>{self.stop().await}
    async fn stop(&mut self)->Result<(),PluginError>{         
        self.state = ChannelState::Stopped;
        self.info("ws: stopping");
        if let Some(tx) = self.shutdown_tx.take() {
            let result = tx.send(());
            println!("@@@ REMOVE: {:?}",result);
        }
        sleep(Duration::from_millis(200));
        if let Some(handle) = self.manager_task.take() {
            handle.abort(); // or optionally: `handle.await.ok();` to wait for graceful exit
        }
        if let Some(handle) = self.ws_task.take() {
            handle.abort(); // or optionally: `handle.await.ok();` to wait for graceful exit
        }
        Ok(()) }
    fn state(&self)->ChannelState{self.state}
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
            node:       Some("ws_in".into()),
            flow:       None,
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
    async fn test_ws_plugin_stop() {
        let mut plugin = WsPlugin::default();

        // Set a dummy logger to satisfy start
        plugin.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn }, LogLevel::Debug);

        // Start the plugin
        let result = plugin.start().await;
        assert!(result.is_ok(), "Plugin failed to start");

        // Stop the plugin
        let stop_result = plugin.stop().await;
        assert!(stop_result.is_ok(), "Plugin failed to stop");

        // Give background task time to log shutdown
        sleep(Duration::from_millis(500));

        // Validate state
        assert_eq!(plugin.state(), ChannelState::Stopped, "Plugin state should be Stopped after stop()");
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
        plugin.set_logger(logger, LogLevel::Debug);

        plugin.start().await.expect("start failed");

        // Give the server a moment to bind and begin listening
        sleep(Duration::from_millis(200));

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