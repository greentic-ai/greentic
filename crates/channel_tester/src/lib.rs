// src/lib.rs
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use notify::{
    event::{CreateKind, ModifyKind},
    Config, Event, EventKind, PollWatcher, RecursiveMode, Watcher,
};
use serde::{Deserialize, Serialize};
use serde_yaml_bw;
use tokio::{fs, sync::{mpsc, Mutex}, task, time};
use uuid::Uuid;

use channel_plugin::{
    export_plugin,
    message::{ChannelCapabilities, ChannelMessage, MessageContent},
    plugin::{ChannelPlugin, ChannelState, DefaultRoutingSupport, LogLevel, PluginError, PluginLogger, RoutingSupport},
};

#[derive(Deserialize, Serialize)]
struct TestFile {
    ygtc: String,
    tests: Vec<SingleTest>,
}

#[derive(Deserialize, Serialize)]
struct SingleTest {
    name: String,
    send: String,
    expect: String,
    timeout: u64,
}

#[derive(Debug)]
pub struct TesterPlugin {
    routing: Arc<DefaultRoutingSupport>,
    logger: Option<PluginLogger>,
    log_level: Option<LogLevel>,
    state: Arc<Mutex<ChannelState>>,
    config: Arc<DashMap<String, String>>,
    incoming_tx: mpsc::UnboundedSender<ChannelMessage>,
    incoming_rx: Arc<Mutex<mpsc::UnboundedReceiver<ChannelMessage>>>,
    reply_tx: mpsc::UnboundedSender<String>,
    reply_rx: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
    watcher: Option<PollWatcher>,
}

impl Clone for TesterPlugin {
    fn clone(&self) -> Self {
        TesterPlugin {
            routing: Arc::clone(&self.routing),
            logger: self.logger.clone(),
            log_level: self.log_level.clone(),
            state: Arc::clone(&self.state),
            config: Arc::clone(&self.config),
            incoming_tx: self.incoming_tx.clone(),
            incoming_rx: Arc::clone(&self.incoming_rx),
            reply_tx: self.reply_tx.clone(),
            reply_rx: Arc::clone(&self.reply_rx),
            watcher: None, // ⛔️ Do NOT clone the watcher
        }
    }
}

impl Default for TesterPlugin {
    fn default() -> Self {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (reply_tx, reply_rx) = mpsc::unbounded_channel();
        TesterPlugin {
            routing: Arc::new(DefaultRoutingSupport::default()),
            logger: None,
            log_level: None,
            state: Arc::new(Mutex::new(ChannelState::Stopped)),
            config: Arc::new(DashMap::new()),
            incoming_tx,
            incoming_rx: Arc::new(Mutex::new(incoming_rx)),
            reply_tx,
            reply_rx: Arc::new(Mutex::new(reply_rx)),
            watcher: None,
        }
    }
}

#[async_trait]
impl ChannelPlugin for TesterPlugin {
    fn name(&self) -> String { "channel_tester".into() }

    fn get_routing_support(&self) -> Option<&dyn RoutingSupport> {
        Some(&*self.routing)
    }

    fn set_logger(&mut self, logger: PluginLogger, log_level: LogLevel) {
        self.logger = Some(logger);
        self.log_level = Some(log_level);
    }

    fn get_logger(&self) -> Option<PluginLogger> {
        self.logger.clone()
    }

    fn get_log_level(&self) -> Option<LogLevel>{
        self.log_level.clone()
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            name:              "channel_tester".into(),
            supports_sending:  true,
            supports_receiving:true,
            supports_text:     true,
            supports_routing:  true,
            ..Default::default()
        }
    }

    fn set_config(&mut self, cfg: DashMap<String, String>) {
        self.config = Arc::new(cfg);
    }

    fn list_config(&self) -> Vec<String> {
        vec!["GREENTIC_DIR".to_string()]
    }

    fn set_secrets(&mut self, _s: DashMap<String, String>) {}

    fn list_secrets(&self) -> Vec<String> { vec![] }

    fn state(&self) -> ChannelState {
        futures::executor::block_on(self.state.lock()).clone()
    }

    async fn start(&mut self) -> Result<(), PluginError> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let greentic_dir = self
            .config
            .get("GREENTIC_DIR")
            .map(|e| PathBuf::from(e.value().as_str()))
            .unwrap_or_else(|| PathBuf::from("./"));
        let tests_dir = greentic_dir.join("./greentic/tests");

        if let Ok(entries) = std::fs::read_dir(&tests_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("test") {
                    let _ = tx.send(path);
                }
            }
        }

        let cfg = Config::default().with_poll_interval(Duration::from_millis(100));
        let mut watcher = PollWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    if matches!(event.kind, EventKind::Create(CreateKind::Any) | EventKind::Modify(ModifyKind::Data(_))) {
                        for path in event.paths {
                            if path.extension().and_then(|s| s.to_str()) == Some("test") {
                                let _ = tx.send(path.clone());
                            }
                        }
                    }
                }
            }, cfg
        ).expect("failed to initialize notify");

        watcher.watch(&tests_dir, RecursiveMode::NonRecursive).expect("failed to watch ./greentic/tests");
        self.watcher = Some(watcher);

        *self.state.lock().await = ChannelState::Running;
        let plugin = Arc::new(self.clone());
        let logger = self.logger.clone().unwrap();

        task::spawn(async move {
            while let Some(path) = rx.recv().await {
                let plugin = plugin.clone();
                let logger = logger.clone();
                task::spawn(async move {
                    let guard = plugin.clone();
                    if let Err(e) = guard.run_test_file(&path).await {
                        logger.log(LogLevel::Error, "tester", &format!("{}: {e:#}", path.display()));
                    }
                });
            }
        });

        #[cfg(test)]
        {

            let echo_rx = self.incoming_rx.clone();
            let echo_plugin = self.clone();

            tokio::spawn(async move {
                loop {
                    let cm = {
                        let mut rx = echo_rx.lock().await;
                        rx.recv().await
                    }.expect("Did not get a channel message");

                    let mut plugin = echo_plugin.clone();

                    let route_match = plugin.routing.find_route(&cm);
                    if let Some(_route) = route_match {
                        use tracing::trace;

                        trace!("✅ Route matched. Echoing message: {:?}", cm);
                        let _ = plugin.send_message(cm).await;
                    } else {
                        use tracing::warn;
                        println!("❌ No matching route for: {:?}", cm);
                        warn!("❌ No matching route for: {:?}", cm);
                    }
/* 
                    let should_route = plugin.routing
                        .find_route(&cm)
                        .map(|r| r.flow == "pass" && r.node == "t_in")
                        .unwrap_or(false);

                    if should_route {
                        println!("✅ Route matched. Echoing message: {:?}", cm);
                        if let Err(e) = plugin.send_message(cm).await {
                            println!("⚠️ Failed to send message: {:?}", e);
                        }
                    } else {
                        println!("❌ No matching route for: {:?}", cm);
                    }

                    */
                }
            });

        }

        Ok(())
    }

    fn drain(&mut self) -> Result<(), PluginError> {
        futures::executor::block_on(async {
            *self.state.lock().await = ChannelState::Draining;
            Ok(())
        })
    }

    async fn wait_until_drained(&mut self, _t: u64) -> Result<(), PluginError> {
        *self.state.lock().await = ChannelState::Stopped;
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), PluginError> {
        *self.state.lock().await = ChannelState::Stopped;
        Ok(())
    }

    async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(), PluginError> {
        if let Some(MessageContent::Text(t)) = msg.content {
            let _ = self.reply_tx.send(t);
        }
        Ok(())
    }

    async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage, PluginError> {
        let mut rx = self.incoming_rx.lock().await;
        match rx.recv().await {
            Some(msg) => Ok(msg),
            None => Err(PluginError::Other("receive_message channel closed".into())),
        }
    }
}

impl TesterPlugin {
    async fn run_test_file(&self, path: &Path) -> anyhow::Result<()> {
        let yaml: String = fs::read_to_string(path).await.expect("could not read test file");
        let tf: TestFile = serde_yaml_bw::from_str(&yaml)?;
        let dest = Path::new("./greentic/flows/running").join(Path::new(&tf.ygtc).file_name().unwrap());
        fs::create_dir_all("./greentic/flows/running").await?;
        fs::copy(&tf.ygtc, &dest).await?;

        let mut results = Vec::new();
        for test in tf.tests {
            let key = Uuid::new_v4().to_string();
            let session_id = format!("tester-session-{}", key);
            let cm = ChannelMessage {
                channel: "tester".into(),
                content: Some(MessageContent::Text(test.send.clone())),
                session_id: Some(session_id.clone()),
                id: key.clone(),
                timestamp: Utc::now(),
                direction: channel_plugin::message::MessageDirection::Incoming,
                ..Default::default()
            };
            let _ = self.incoming_tx.send(cm);

            let expected = test.expect.clone();
            let got = time::timeout(Duration::from_millis(test.timeout), async {
                let mut rx = self.reply_rx.lock().await;
                loop {
                    let Some(msg) = rx.recv().await else { break None };
                    if msg == expected {
                        break Some(msg);
                    }
                }
            }).await.ok().flatten();

            let pass = got.as_deref() == Some(&test.expect);
            results.push(format!("{}: {}", test.name, if pass {"PASS"} else {"FAIL"}));

            if !pass {
                println!("❌ Test `{}` failed: expected {:?}, got {:?}", test.name, test.expect, got);
            }
        }

        let out = path.with_extension("test.result");
        fs::write(&out, results.join("\n")).await?;
        Ok(())
    }
}

export_plugin!(TesterPlugin);


// In src/lib.rs (or tests/integration_tests.rs)

#[cfg(test)]
mod tests {
    use crate::{SingleTest, TestFile};

    use super::TesterPlugin;
    use channel_plugin::message::{RouteBinding, RouteMatcher};
    use channel_plugin::plugin::{ChannelPlugin, PluginLogger, LogLevel};
    use tempfile::TempDir;
    use tokio::fs;
    use tokio::time::{self, Duration};
    use tokio::io::AsyncWriteExt;
    use dashmap::DashMap;
    use std::env;
    use std::ffi::CStr;
    use std::path::PathBuf;
    use std::io::Write;

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

    /// A no-op logger so we don’t spam stdout
    extern "C" fn noop_log(_: *mut std::ffi::c_void, _: LogLevel, _: *const i8, _: *const i8) {}
    fn make_logger() -> PluginLogger {
        PluginLogger { ctx: std::ptr::null_mut(), log_fn: noop_log }
    }

    

    // helper: write the dummy flow file that simply echoes tester_in → tester_out
    async fn write_minimal_flow(dir: &TempDir, name: &str) -> PathBuf {
        let flows = dir.path().join("greentic/flows");
        fs::create_dir_all(&flows).await.unwrap();
        let ygtc_path = flows.join(name);
        let yaml = r#"
id: pass
channels:
  - tester
nodes:
  t_in:
    channel: tester
    in: true
  t_out:
    channel: tester
    out: true
connections:
  - from: t_in
    to: t_out
"#;
        let mut f = fs::File::create(&ygtc_path).await.unwrap();
        f.write_all(yaml.as_bytes()).await.unwrap();
        ygtc_path
    }

    // helper: write the .test file
    async fn write_test_file(dir: &TempDir, flow_rel: &str, tests: &str) -> PathBuf {
        let tests_dir = dir.path().join("greentic/tests");
        fs::create_dir_all(&tests_dir).await.unwrap();
        let test_path = tests_dir.join("foo.test");
        let content = format!(
            r#"
ygtc: "{flow}"
tests:
  - name: "roundtrip"
    send: "{send}"
    expect: "{expect}"
    timeout: 1000
"#,
            flow = flow_rel,
            send = tests,
            expect = tests,
        );
        let mut f = fs::File::create(&test_path).await.unwrap();
        f.write_all(content.as_bytes()).await.unwrap();
        test_path
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_run_test_file_pass() {
        let tmp = TempDir::new().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

       
        // create the directories that run_test_file expects
        fs::create_dir_all("greentic/tests").await.unwrap();
        fs::create_dir_all("greentic/flows").await.unwrap(); 

        // 1) write minimal flow
        let _ygtc = write_minimal_flow(&tmp, "pass.ygtc").await;
        // 2) write test file pointing to it
        let test_path = write_test_file(&tmp, "./greentic/flows/pass.ygtc", "hello").await;

        // 3) new plugin
        let mut plugin = TesterPlugin::default();
        plugin.add_route(RouteBinding {
            matcher: RouteMatcher::Custom("hello".into()),
            flow: "pass".into(),
            node: "t_in".into(),
        });
        plugin.set_logger(make_logger(), LogLevel::Debug);
        plugin.start().await.expect("start");

        // 4) invoke run_test_file directly
        plugin.run_test_file(&test_path)
              .await
              .expect("run_test_file");

        // 5) read the .result file
        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(out.contains("roundtrip: PASS"),
            "expected PASS but got: {}", out);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_run_test_file_fail() {
        let tmp = TempDir::new().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        // create the directories that run_test_file expects
        fs::create_dir_all("greentic/tests").await.unwrap();
        fs::create_dir_all("greentic/flows").await.unwrap(); 

        // same flow
        let _ygtc = write_minimal_flow(&tmp, "pass.ygtc").await;
        // but expect something else (will fail)
        //let _tests_dir = tmp.path().join("greentic/tests");
        let test_path = write_test_file(&tmp, "./greentic/flows/pass.ygtc", "ping").await;
        // override the expect to something wrong:
        // (just rewrite file)
        let mut f = fs::File::create(&test_path).await.unwrap();
        let bad = r#"
ygtc: "./greentic/flows/pass.ygtc"
tests:
  - name: "roundtrip"
    send: "ping"
    expect: "PONG"
    timeout: 100
"#;
        f.write_all(bad.as_bytes()).await.unwrap();

        let mut plugin = TesterPlugin::default();
        plugin.add_route(RouteBinding {
            matcher: RouteMatcher::Custom("ping".into()),
            flow: "pass".into(),
            node: "t_in".into(),
        });
        plugin.set_logger(make_logger(), LogLevel::Debug);
        plugin.start().await.expect("start");

        plugin.clone()
              .run_test_file(&test_path)
              .await
              .expect("run_test_file");

        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(out.contains("roundtrip: FAIL"),
            "expected FAIL but got: {}", out);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_watcher_picks_up_and_runs() -> anyhow::Result<()> {
        // 1) Create a temporary working dir and cd into it
        let tmp = TempDir::new()?;
        env::set_current_dir(&tmp)?;


        // create the directories that run_test_file expects
        fs::create_dir_all("greentic/flows").await.unwrap(); 

        // 2) Create the tests & flows/running directories
        fs::create_dir_all("greentic/tests").await?;
        fs::create_dir_all("greentic/flows/running").await?;

        // 3) Write a minimal flow (ygtc) that simply echoes through tester
        //    We'll point our TestFile at this dummy .ygtc
        let flow_path = tmp.path().join("simple.ygtc");
        {
            let mut f = std::fs::File::create(&flow_path)?;
            writeln!(f, r#"
id: dummy
title: dummy
description: test flow
channels:
  - tester
nodes:
  tester_in:
    channel: tester
    in: true
  tester_out:
    channel: tester
    out: true
connections:
  tester_in:
    - tester_out
"#)?;
        }

        // 4) Build a .test file referencing that flow
        let test = TestFile {
            ygtc: flow_path.to_string_lossy().into_owned(),
            tests: vec![ SingleTest {
                name:     "roundtrip".into(),
                send:     "hello".into(),
                expect:   "hello".into(),
                timeout:  500,
            } ],
        };
        let test_yaml = serde_yaml_bw::to_string(&test)?;
        let test_path = PathBuf::from("greentic/tests/my_test.test");
        fs::write(&test_path, test_yaml).await?;

        // 5) Start the plugin
        let mut plugin = TesterPlugin::default();
        let config = DashMap::<String,String>::new();
        config.insert("GREENTIC_DIR".to_string(),tmp.path().to_string_lossy().into());
        plugin.set_config(config);
        // give it a no‐op logger so it won't panic
        plugin.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn }, LogLevel::Debug);
        plugin.start().await?;

        // 6) Wait up to 1s for the .test.result file to appear
        let mut interval = time::interval(Duration::from_millis(100));
        let mut found = false;
        for _ in 0..20 {
            interval.tick().await;
            if tmp.path().join("greentic/tests/my_test.test.result").exists() {
                found = true;
                break;
            }
        }
        assert!(found, "expected my_test.test.result to be written");

        // 7) Read & verify its contents
        let result = fs::read_to_string("greentic/tests/my_test.test.result").await?;
        assert!(result.contains("roundtrip: PASS"), "got result = {:?}", result);

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_custom_matcher_no_route() {
        let tmp = TempDir::new().unwrap();
        env::set_current_dir(tmp.path()).unwrap();

        fs::create_dir_all("greentic/tests").await.unwrap();
        fs::create_dir_all("greentic/flows").await.unwrap(); 

        let _ygtc = write_minimal_flow(&tmp, "pass.ygtc").await;
        let test_path = write_test_file(&tmp, "./greentic/flows/pass.ygtc", "unexpected").await;

        let mut plugin = TesterPlugin::default();
        plugin.set_logger(make_logger(), LogLevel::Debug);

        // 🔸 No route added for "unexpected"

        plugin.start().await.expect("start");
        plugin.add_route(RouteBinding {
            matcher: RouteMatcher::Custom("your_test_input_string".into()),
            flow: "pass".into(),
            node: "t_in".into(),
        });

        plugin.clone()
            .run_test_file(&test_path)
            .await
            .expect("run_test_file");

        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(out.contains("roundtrip: FAIL"),
            "expected FAIL due to missing route but got: {}", out);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_custom_matcher_multiple_routes() {
        let tmp = TempDir::new().unwrap();
        env::set_current_dir(tmp.path()).unwrap();

        fs::create_dir_all("greentic/tests").await.unwrap();
        fs::create_dir_all("greentic/flows").await.unwrap(); 

        let _ygtc = write_minimal_flow(&tmp, "pass.ygtc").await;
        let test_path = write_test_file(&tmp, "./greentic/flows/pass.ygtc", "target").await;

        let mut plugin = TesterPlugin::default();
        plugin.set_logger(make_logger(), LogLevel::Debug);

        plugin.add_route(RouteBinding {
            matcher: RouteMatcher::Custom("wrong".into()),
            flow: "fail".into(),
            node: "fail_node".into(),
        });

        plugin.add_route(RouteBinding {
            matcher: RouteMatcher::Custom("target".into()),
            flow: "pass".into(),
            node: "t_in".into(),
        });

        plugin.start().await.expect("start");

        plugin.clone()
            .run_test_file(&test_path)
            .await
            .expect("run_test_file");

        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(out.contains("roundtrip: PASS"),
            "expected PASS from correct route match but got: {}", out);
    }
}
