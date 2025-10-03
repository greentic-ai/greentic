// src/lib.rs
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use anyhow::{anyhow, Context};
use chrono::Utc;
use dashmap::DashMap;
use notify::{
    Config, Event, EventKind, PollWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind},
};
use serde::{Deserialize, Serialize};
use serde_yaml_bw;
use std::sync::Mutex as SMutex;
use tokio::{
    fs,
    sync::{Mutex, Notify, mpsc},
    task, time,
};
use tracing::{error, info};
use uuid::Uuid;

use channel_plugin::{
    message::{
        CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult,
        InitParams, InitResult, ListKeysResult, MessageContent, MessageInResult, MessageOutParams,
        MessageOutResult, NameResult, StateResult, StopResult, PLUGIN_VERSION,
    },
    plugin_runtime::{run, HasStore, PluginHandler},
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
    state: Arc<SMutex<ChannelState>>,
    config: DashMap<String, String>,
    secrets: DashMap<String, String>,
    incoming_tx: mpsc::UnboundedSender<ChannelMessage>,
    incoming_rx: Arc<Mutex<mpsc::UnboundedReceiver<ChannelMessage>>>,
    reply_tx: mpsc::UnboundedSender<String>,
    reply_rx: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
    watcher: Option<PollWatcher>,
    shutdown: Option<Arc<tokio::sync::Notify>>,
}

impl Clone for TesterPlugin {
    fn clone(&self) -> Self {
        TesterPlugin {
            state: Arc::new(SMutex::new(self.state.lock().unwrap().clone())),
            config: self.config.clone(),
            secrets: self.secrets.clone(),
            incoming_tx: self.incoming_tx.clone(),
            incoming_rx: Arc::clone(&self.incoming_rx),
            reply_tx: self.reply_tx.clone(),
            reply_rx: Arc::clone(&self.reply_rx),
            watcher: None, // ⛔️ Do NOT clone the watcher
            shutdown: self.shutdown.clone(),
        }
    }
}

impl Default for TesterPlugin {
    fn default() -> Self {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (reply_tx, reply_rx) = mpsc::unbounded_channel();
        TesterPlugin {
            state: Arc::new(SMutex::new(ChannelState::STOPPED)),
            config: DashMap::new(),
            secrets: DashMap::new(),
            incoming_tx,
            incoming_rx: Arc::new(Mutex::new(incoming_rx)),
            reply_tx,
            reply_rx: Arc::new(Mutex::new(reply_rx)),
            watcher: None,
            shutdown: None,
        }
    }
}

impl HasStore for TesterPlugin {
    fn config_store(&self) -> &DashMap<String, String> {
        &self.config
    }
    fn secret_store(&self) -> &DashMap<String, String> {
        &self.secrets
    }
}

#[async_trait]
impl PluginHandler for TesterPlugin {
    fn name(&self) -> NameResult {
        NameResult {
            name: "tester".to_string(),
        }
    }

    fn capabilities(&self) -> CapabilitiesResult {
        CapabilitiesResult {
            capabilities: ChannelCapabilities {
                name: "tester".into(),
                version: PLUGIN_VERSION.to_string(),
                supports_sending: true,
                supports_receiving: true,
                supports_text: true,
                supports_routing: true,
                /* … */
                ..Default::default()
            },
        }
    }

    fn list_config_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![(
                "GREENTIC_DIR".to_string(),
                Some("The directory where greentic is installed".to_string()),
            )],
            optional_keys: vec![],
            dynamic_keys: vec![],
        }
    }

    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![],
            optional_keys: vec![],
            dynamic_keys: vec![],
        }
    }
    async fn state(&self) -> StateResult {
        StateResult {
            state: self.state.lock().unwrap().clone(),
        }
    }

    async fn init(&mut self, _params: InitParams) -> InitResult {
        let shutdown = Arc::new(Notify::new());
        self.shutdown = Some(shutdown.clone());
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
                    if matches!(
                        event.kind,
                        EventKind::Create(CreateKind::Any) | EventKind::Modify(ModifyKind::Data(_))
                    ) {
                        for path in event.paths {
                            if path.extension().and_then(|s| s.to_str()) == Some("test") {
                                let _ = tx.send(path.clone());
                            }
                        }
                    }
                }
            },
            cfg,
        )
        .expect("failed to initialize notify");

        watcher
            .watch(&tests_dir, RecursiveMode::NonRecursive)
            .expect("failed to watch ./greentic/tests");
        self.watcher = Some(watcher);

        *self.state.lock().unwrap() = ChannelState::RUNNING;
        let plugin = Arc::new(self.clone());

        let shutdown_clone = shutdown.clone();
        task::spawn(async move {
            loop {
                tokio::select! {
                    maybe_path = rx.recv() => {
                        let plugin = plugin.clone();
                        task::spawn(async move {
                            let guard = plugin.clone();
                            if let Some(path_buf) = maybe_path {
                                let path = Path::new(&path_buf);
                                if let Err(e) = guard.run_test_file(&path).await {
                                    error!("{}: {e:#}", path.display());
                                }
                            }
                        });
                    }
                    _ = shutdown_clone.notified() => {
                        info!("✅ Shutting down watcher receive loop");
                        return;
                    }
                }
            }
        });

        #[cfg(test)]
        {
            let echo_rx = self.incoming_rx.clone();
            let echo_plugin = self.clone();

            let shutdown_test = shutdown.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown_test.notified() => {
                            info!("✅ Shutting down echo responder");
                            return;
                        }
                        maybe_cm = async {
                            let mut rx = echo_rx.lock().await;
                            rx.recv().await
                        } => {
                            if let Some(cm) = maybe_cm {
                                let mut plugin = echo_plugin.clone();
                                let _ = plugin.send_message(MessageOutParams { message: cm }).await;
                            } else {
                                // Channel closed
                                break;
                            }
                        }
                    }
                }
            });
        }

        InitResult {
            success: true,
            error: None,
        }
    }

    async fn drain(&mut self) -> DrainResult {
        if let Some(watcher) = self.watcher.take() {
            drop(watcher); // this stops the thread
        }
        if let Some(shutdown) = self.shutdown.take() {
            shutdown.notify_waiters();
        }
        *self.state.lock().unwrap() = ChannelState::STOPPED;
        DrainResult {
            success: true,
            error: None,
        }
    }

    async fn stop(&mut self) -> StopResult {
        if let Some(watcher) = self.watcher.take() {
            drop(watcher); // this stops the thread
        }
        if let Some(shutdown) = self.shutdown.take() {
            shutdown.notify_waiters();
        }
        *self.state.lock().unwrap() = ChannelState::STOPPED;
        StopResult {
            success: true,
            error: None,
        }
    }

    async fn send_message(&mut self, params: MessageOutParams) -> MessageOutResult {
        for content in &params.message.content {
            if let MessageContent::Text { text: t } = content {
                let _ = self.reply_tx.send(t.to_string());
            }
        }
        info!("got a new message {:?}", params.message);
        MessageOutResult {
            success: true,
            error: None,
        }
    }

    async fn receive_message(&mut self) -> MessageInResult {
        info!("receive_message");
        let mut rx = self.incoming_rx.lock().await;
        match rx.recv().await {
            Some(msg) => MessageInResult {
                message: msg,
                error: false,
            },
            None => {
                error!("receive_message channel closed");
                return MessageInResult {
                    message: ChannelMessage::default(),
                    error: true,
                };
            }
        }
    }
}

impl TesterPlugin {
    async fn run_test_file(&self, path: &Path) -> anyhow::Result<()> {
        let yaml: String = fs::read_to_string(path)
            .await
            .expect("could not read test file");
        let tf: TestFile = serde_yaml_bw::from_str(&yaml)?;

        let source_candidate = PathBuf::from(&tf.ygtc);
        let greentic_dir = self
            .config
            .get("GREENTIC_DIR")
            .map(|e| PathBuf::from(e.value().as_str()))
            .unwrap_or_else(|| PathBuf::from("."));

        let source = if source_candidate.is_absolute() {
            source_candidate
        } else if fs::metadata(&source_candidate).await.is_ok() {
            source_candidate
        } else {
            greentic_dir.join(&source_candidate)
        };

        fs::metadata(&source)
            .await
            .with_context(|| format!("Flow file not found at {}", source.display()))?;

        let flows_running = PathBuf::from("./greentic/flows/running");
        fs::create_dir_all(&flows_running).await?;
        let dest = flows_running.join(
            source
                .file_name()
                .ok_or_else(|| anyhow!("Flow file missing name: {}", source.display()))?,
        );
        fs::copy(&source, &dest).await?;

        let mut results = Vec::new();
        for test in tf.tests {
            let key = Uuid::new_v4().to_string();
            let session_id = format!("tester-session-{}", key);
            let cm = ChannelMessage {
                channel: "tester".into(),
                content: vec![MessageContent::Text {
                    text: test.send.clone(),
                }],
                session_id: Some(session_id.clone()),
                id: key.clone(),
                timestamp: Utc::now().to_rfc3339(),
                direction: channel_plugin::message::MessageDirection::Incoming,
                ..Default::default()
            };
            let _ = self.incoming_tx.send(cm);

            let expected = test.expect.clone();
            let got = time::timeout(Duration::from_millis(test.timeout), async {
                let mut rx = self.reply_rx.lock().await;
                loop {
                    let Some(msg) = rx.recv().await else {
                        break None;
                    };
                    if msg == expected {
                        break Some(msg);
                    }
                }
            })
            .await
            .ok()
            .flatten();

            let pass = got.as_deref() == Some(&test.expect);
            results.push(format!(
                "{}: {}",
                test.name,
                if pass { "PASS" } else { "FAIL" }
            ));

            if !pass {
                println!(
                    "❌ Test `{}` failed: expected {:?}, got {:?}",
                    test.name, test.expect, got
                );
            }
        }

        let out = path.with_extension("test.result");
        fs::write(&out, results.join("\n")).await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(TesterPlugin::default()).await
}
// In src/lib.rs (or tests/integration_tests.rs)

#[cfg(test)]
mod tests {
    use crate::{SingleTest, TestFile};

    use super::TesterPlugin;
    use channel_plugin::message::{InitParams, LogLevel};
    use channel_plugin::plugin_runtime::{PluginHandler, VERSION};
    use std::env;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;
    use tokio::io::AsyncWriteExt;
    use tokio::time::{self, Duration};

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
        let params = InitParams {
            version: VERSION.to_string(),
            config: vec![("GREENTIC_DIR".to_string(), "./greentic".to_string())],
            secrets: vec![],
            log_level: LogLevel::Info,
            log_dir: Some("./logs".to_string()),
            otel_endpoint: None,
        };
        let result = plugin.init(params).await;
        assert!(result.success);

        // 4) invoke run_test_file directly
        plugin
            .run_test_file(&test_path)
            .await
            .expect("run_test_file");

        // 5) read the .result file
        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(
            out.contains("roundtrip: PASS"),
            "expected PASS but got: {}",
            out
        );
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

        let params = InitParams {
            version: VERSION.to_string(),
            config: vec![("GREENTIC_DIR".to_string(), "./greentic".to_string())],
            secrets: vec![],
            log_level: LogLevel::Info,
            log_dir: Some("./logs".to_string()),
            otel_endpoint: None,
        };
        let result = plugin.init(params).await;
        assert!(result.success);

        plugin
            .clone()
            .run_test_file(&test_path)
            .await
            .expect("run_test_file");

        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(
            out.contains("roundtrip: FAIL"),
            "expected FAIL but got: {}",
            out
        );
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
            writeln!(
                f,
                r#"
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
"#
            )?;
        }

        // 4) Build a .test file referencing that flow
        let test = TestFile {
            ygtc: flow_path.to_string_lossy().into_owned(),
            tests: vec![SingleTest {
                name: "roundtrip".into(),
                send: "hello".into(),
                expect: "hello".into(),
                timeout: 500,
            }],
        };
        let test_yaml = serde_yaml_bw::to_string(&test)?;
        let test_path = PathBuf::from("greentic/tests/my_test.test");
        fs::write(&test_path, test_yaml).await?;

        // 5) Start the plugin
        let mut plugin = TesterPlugin::default();
        let params = InitParams {
            version: VERSION.to_string(),
            config: vec![("GREENTIC_DIR".to_string(), "./greentic".to_string())],
            secrets: vec![],
            log_level: LogLevel::Info,
            log_dir: Some("./logs".to_string()),
            otel_endpoint: None,
        };
        let result = plugin.init(params).await;
        assert!(result.success);

        // 6) Wait up to 1s for the .test.result file to appear
        let mut interval = time::interval(Duration::from_millis(100));
        let mut found = false;
        for _ in 0..20 {
            interval.tick().await;
            if tmp
                .path()
                .join("greentic/tests/my_test.test.result")
                .exists()
            {
                found = true;
                break;
            }
        }
        assert!(found, "expected my_test.test.result to be written");

        // 7) Read & verify its contents
        let result = fs::read_to_string("greentic/tests/my_test.test.result").await?;
        assert!(
            result.contains("roundtrip: PASS"),
            "got result = {:?}",
            result
        );

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

        // 🔸 No route added for "unexpected"

        let params = InitParams {
            version: VERSION.to_string(),
            config: vec![("GREENTIC_DIR".to_string(), "./greentic".to_string())],
            secrets: vec![],
            log_level: LogLevel::Info,
            log_dir: Some("./logs".to_string()),
            otel_endpoint: None,
        };
        let result = plugin.init(params).await;
        assert!(result.success);

        plugin
            .clone()
            .run_test_file(&test_path)
            .await
            .expect("run_test_file");

        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(
            out.contains("roundtrip: FAIL"),
            "expected FAIL due to missing route but got: {}",
            out
        );
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
        let params = InitParams {
            version: VERSION.to_string(),
            config: vec![(
                "GREENTIC_DIR".to_string(),
                tmp.path().to_string_lossy().into(),
            )],
            secrets: vec![],
            log_level: LogLevel::Info,
            log_dir: Some("./logs".to_string()),
            otel_endpoint: None,
        };
        let result = plugin.init(params).await;
        assert!(result.success);

        plugin
            .clone()
            .run_test_file(&test_path)
            .await
            .expect("run_test_file");

        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(
            out.contains("roundtrip: PASS"),
            "expected PASS from correct route match but got: {}",
            out
        );
    }
}
