// src/lib.rs
use std::{
    collections::HashMap, path::Path, sync::Arc, time::Duration
};
use async_trait::async_trait;
use chrono::Utc;
use notify::{event::{CreateKind, ModifyKind}, Config, Event, EventKind, PollWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_yaml_bw;
use tokio::{
    fs, sync::{broadcast, mpsc, Mutex}, task, time
};
use channel_plugin::{
    export_plugin,
    message::{ChannelCapabilities, ChannelMessage, MessageContent},
    plugin::{ChannelPlugin, ChannelState, LogLevel, PluginError, PluginLogger},
};
use anyhow::Context;
use uuid::Uuid;

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

/// A plugin that watches `./greentic/tests/*.test` and drives a suite of send/receive checks.
pub struct TesterPlugin {
    logger:    Option<PluginLogger>,
    state:     ChannelState,
    watcher: Option<PollWatcher>,
    config: HashMap<String,String>,

    /// “Incoming” → host calls receive_message()  
    incoming_tx:   broadcast::Sender<ChannelMessage>,  


    /// “Outgoing” → host calls our send_message()  
    reply_tx:      broadcast::Sender<String>, 


}

impl Default for TesterPlugin {
    fn default() -> Self {
       let (incoming_tx, _) = broadcast::channel(32);
        let (reply_tx,    _) = broadcast::channel(32);
        
        TesterPlugin {
            logger:     None,
            state:      ChannelState::Stopped,
            config: HashMap::new(),
            incoming_tx,
            reply_tx,
            watcher: None,
        }
    }
}

#[async_trait]
impl ChannelPlugin for TesterPlugin {
    fn name(&self) -> String { "channel_tester".into() }

    fn set_logger(&mut self, logger: PluginLogger) {
        self.logger = Some(logger);
    }

    fn get_logger(&self) -> Option<PluginLogger> {
        self.logger.clone()
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            name:              "channel_tester".into(),
            supports_sending:  true,
            supports_receiving:true,
            supports_text:     true,
            ..Default::default()
        }
    }

    fn set_config(&mut self, cfg: HashMap<String, String>) { 
        self.config = cfg;
    }

    fn list_config(&self) -> Vec<String> { 
        vec!["GREENTIC_DIR".to_string()]
     }

    fn set_secrets(&mut self, _s: HashMap<String, String>) { }

    fn list_secrets(&self) -> Vec<String> { vec![] }

    fn state(&self) -> ChannelState { self.state.clone() }

    async fn start(&mut self) -> Result<(), PluginError> {
        let (tx, mut rx) = mpsc::unbounded_channel();
    println!("@@@ REMOVE 1");
        // spawn a filesystem watcher in a blocking task
        let greentic_dir = Path::new(self.config.get("GREENTIC_DIR").map(String::as_str).unwrap_or("./"));
        let tests_dir = greentic_dir.join("./greentic/tests");

        // 0) BOOTSTRAP: on startup, scan for existing *.test files
        if let Ok(entries) = std::fs::read_dir(&tests_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("test") {
                    let _ = tx.send(path);
                }
            }
        }


        // 1) now set up your PollWatcher
        let cfg = Config::default().with_poll_interval(Duration::from_millis(100));
        let mut watcher = PollWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                    println!("@@@ REMOVE 2: {:?}", event);
                if let EventKind::Create(CreateKind::Any) | EventKind::Modify(ModifyKind::Data(_)) = event.kind {
                    for path in event.paths {
                         println!("@@@ REMOVE 2.5: {:?}", path);
                        if path.extension().and_then(|s| s.to_str()) == Some("test") {
                                println!("@@@ REMOVE 3");
                            let _ = tx.send(path.clone());
                        }
                    }
                }
            }
        }, cfg,)
        .context("failed to initialize notify")?;
        watcher
            .watch(&tests_dir, RecursiveMode::NonRecursive)
            .context("failed to watch ./greentic/tests")?;
        self.watcher = Some(watcher);
    println!("@@@ REMOVE 4");
        self.state = ChannelState::Running;
        let plugin = Arc::new(Mutex::new(self.clone_inner()));
        let logger = self.logger.clone().unwrap();

        task::spawn(async move {
            while let Some(path) = rx.recv().await {
                    println!("@@@ REMOVE 5");
                let mut guard = plugin.lock().await;
                if let Err(e) = guard.run_test_file(&path).await {
                        println!("@@@ REMOVE 6");
                    logger.log(LogLevel::Error, "tester", &format!("{}: {e:#}", path.display()));
                }
            }
        });

        // only in `cargo test` do we auto-echo back into send_message()
        #[cfg(test)]
        {
            let mut echo_rx = self.incoming_tx.subscribe();
            let mut echo_plugin = self.clone_inner();
            tokio::task::spawn(async move {
                while let Ok(cm) = echo_rx.recv().await {
                    println!("@@@ REMOVE TEST");
                    let _ = echo_plugin.send_message(cm).await;
                }
            });
        }

        Ok(())
    }

    fn drain(&mut self) -> Result<(), PluginError> {
        self.state = ChannelState::Draining;
        Ok(())
    }

    async fn wait_until_drained(&mut self, _t: u64) -> Result<(), PluginError> {
        self.state = ChannelState::Stopped;
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), PluginError> {
        self.state = ChannelState::Stopped;
        Ok(())
    }

    async fn send_message(&mut self, msg: ChannelMessage) -> anyhow::Result<(), PluginError> {
            println!("@@@ REMOVE 7");
        if let Some(MessageContent::Text(t)) = msg.content {
            // this will broadcast to *all* subscribers; your test harness
            // will call `.subscribe()` to get the next one.
            let _ = self.reply_tx.send(t);
        }
        Ok(())
    }

    async fn receive_message(&mut self) -> anyhow::Result<ChannelMessage, PluginError> {
            println!("@@@ REMOVE 8");
        // each call to receive_message() should get its own subscriber:
        let mut rx = self.incoming_tx.subscribe();
        let result = rx.recv().await.map_err(|_| PluginError::Other("tester rx closed".into()));
        println!("@@@ REMOVE 8.5:{:?}",result);
        result
    }
}
impl TesterPlugin {
    /// We need `clone_inner` because our `start` took `&mut self`, but
    /// we want an `Arc<Mutex<TesterPlugin>>`.
    fn clone_inner(&self) -> TesterPlugin {
        TesterPlugin {
            logger:     self.logger.clone(),
            state:      self.state.clone(),
            config:     self.config.clone(),
            incoming_tx: self.incoming_tx.clone(),
            reply_tx:    self.reply_tx.clone(),
            watcher:    None,
        }
    }

    /// Actually parse & drive one `.test` file.
    async fn run_test_file(&mut self, path: &Path) -> anyhow::Result<()> {
            println!("@@@ REMOVE 9");
        let logger = self.logger.clone().unwrap();
        logger.log(LogLevel::Info, "tester", &format!("⏳ loading {}", path.display()));
        let yaml: String = fs::read_to_string(path).await.expect("could not read test file");
        let tf: TestFile = serde_yaml_bw::from_str(&yaml)?;
    println!("@@@ REMOVE 10");
        // copy flow
        let dest = Path::new("./greentic/flows/running")
            .join(Path::new(&tf.ygtc).file_name().unwrap());
        fs::create_dir_all("./greentic/flows/running").await?;
        fs::copy(&tf.ygtc, &dest).await?;
    println!("@@@ REMOVE 11");
        let mut results = Vec::new();
        for test in tf.tests {
            logger.log(LogLevel::Info, "tester", &format!("→ `{}` sending “{}”", test.name, test.send));

            // stage the incoming ChannelMessage
            let mut cm = ChannelMessage::default();
            cm.channel    = "tester".into();
            cm.content    = Some(MessageContent::Text(test.send.clone()));
            cm.session_id = Some("tester".into());
            cm.id         = Uuid::new_v4().to_string();
            cm.timestamp  = Utc::now();
            cm.direction  = /* incoming */ channel_plugin::message::MessageDirection::Incoming;
            self.incoming_tx.send(cm).ok();
    println!("@@@ REMOVE 12");
            // now wait for the flow to reply via our `send_message` hook...
            let mut reply_sub = self.reply_tx.subscribe();
            let got = time::timeout(Duration::from_millis(test.timeout), async {
                reply_sub.recv().await.map(Some).unwrap_or(None)
            })
            .await
            .unwrap_or(None);

            let pass = got.as_deref() == Some(&test.expect);
            results.push(format!("{}: {}", test.name, if pass {"PASS"} else {"FAIL"}));
            logger.log(
                LogLevel::Info,
                "tester",
                &format!("→ `{}` {} (got {:?})", test.name, if pass {"PASS"} else {"FAIL"}, got),
            );
    println!("@@@ REMOVE 13");
            // drain any extra replies before next test:
            while let Ok(Ok(_)) = time::timeout(Duration::from_millis(0), reply_sub.recv()).await {
                continue;
            }
        }

        // write results
        let out = path.with_extension("test.result");
        fs::write(&out, results.join("\n")).await?;
        logger.log(LogLevel::Info, "tester", &format!("wrote {}", out.display()));
        Ok(())
    }
}
export_plugin!(TesterPlugin);

// In src/lib.rs (or tests/integration_tests.rs)

#[cfg(test)]
mod tests {
    use crate::{SingleTest, TestFile};

    use super::TesterPlugin;
    use channel_plugin::plugin::{ChannelPlugin, PluginLogger, LogLevel};
    use tempfile::TempDir;
    use tokio::fs;
    use tokio::time::{self, Duration};
    use tokio::io::AsyncWriteExt;
    use std::collections::HashMap;
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

    #[tokio::test]
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
        plugin.set_logger(make_logger());
        plugin.start().await.expect("start");

        // 4) invoke run_test_file directly
        plugin.clone_inner()
              .run_test_file(&test_path)
              .await
              .expect("run_test_file");

        // 5) read the .result file
        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(out.contains("roundtrip: PASS"),
            "expected PASS but got: {}", out);
    }

    #[tokio::test]
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
        plugin.set_logger(make_logger());
        plugin.start().await.expect("start");

        plugin.clone_inner()
              .run_test_file(&test_path)
              .await
              .expect("run_test_file");

        let result_path = test_path.with_extension("test.result");
        let out = fs::read_to_string(&result_path).await.unwrap();
        assert!(out.contains("roundtrip: FAIL"),
            "expected FAIL but got: {}", out);
    }

    #[tokio::test]
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
        let mut config = HashMap::<String,String>::new();
        config.insert("GREENTIC_DIR".to_string(),tmp.path().to_string_lossy().into());
        plugin.set_config(config);
        // give it a no‐op logger so it won't panic
        plugin.set_logger(PluginLogger { ctx: std::ptr::null_mut(), log_fn: test_log_fn });
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
}
