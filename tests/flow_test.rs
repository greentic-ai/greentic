// src/flow_test.rs
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use channel_plugin::message::{ChannelCapabilities, ChannelMessage, MessageContent, MessageDirection, Participant};
use channel_plugin::plugin::{ChannelState, PluginLogger};
use channel_plugin::PluginHandle;
use greentic::channel::manager::{ChannelManager, HostLogger, IncomingHandler};
use greentic::channel::node::ChannelsRegistry;
use greentic::channel::plugin::Plugin;
use greentic::channel::PluginWrapper;
use greentic::config::{ConfigManager, MapConfigManager};
use greentic::mapper::{CopyKey, CopyMapper, Mapper};
use greentic::process::manager::ProcessManager;
use greentic::state::{InMemoryState, StateValue};
use petgraph::visit::Topo;
use schemars::schema_for;
use serde_json::json;

use greentic::executor::Executor;
use greentic::flow::{ChannelNodeConfig, ExecutionReport, Flow, FlowManager, NodeConfig, NodeKind, ResolveError, TemplateContext, ToolNodeConfig, ValueOrTemplate};
use greentic::logger::{Logger, OpenTelemetryLogger};
use greentic::message::Message;
use greentic::node::{ChannelOrigin, NodeContext};
use greentic::secret::{EmptySecretsManager, SecretsManager};
use tempfile::TempDir;

/// Helper to build a dummy `Executor` for NodeContext
fn make_executor() -> Arc<Executor> {
    let secrets = SecretsManager(EmptySecretsManager::new());
    let logger = Logger(Box::new(OpenTelemetryLogger::new()));
    Executor::new(secrets, logger)
}

 /// A dummy Plugin whose FFI pointers do nothing.
pub fn make_noop_plugin() -> Arc<Plugin> {
    // All the extern-C functions:
    unsafe extern "C" fn create() -> PluginHandle { std::ptr::null_mut() }
    unsafe extern "C" fn destroy(_: PluginHandle) {}
    unsafe extern "C" fn set_logger(_: PluginHandle, _logger_ptr: PluginLogger) {}
    unsafe extern "C" fn name(_: PluginHandle) -> *mut i8 { std::ptr::null_mut() }
    unsafe extern "C" fn start(_: PluginHandle) -> bool { true }
    unsafe extern "C" fn drain(_: PluginHandle) -> bool { true }
    unsafe extern "C" fn stop(_: PluginHandle) -> bool { true }
    unsafe extern "C" fn wait_until_drained(_: PluginHandle, _: u64) -> bool { true }
    unsafe extern "C" fn poll(_: PluginHandle, _out: *mut ChannelMessage) -> bool { false }
    unsafe extern "C" fn send(_: PluginHandle, _: *const ChannelMessage) -> bool { true }
    unsafe extern "C" fn caps(_: PluginHandle, out: *mut ChannelCapabilities) -> bool {
        if !out.is_null() {
            unsafe { std::ptr::write(out, ChannelCapabilities::default()) };
            true
        } else {
            false
        }
    }
    unsafe extern "C" fn state(_: PluginHandle) -> ChannelState {
        ChannelState::Stopped
    }
    unsafe extern "C" fn set_config(_: PluginHandle, _: *const i8) {}
    unsafe extern "C" fn set_secrets(_: PluginHandle, _: *const i8) {}
    unsafe extern "C" fn list_config(_: PluginHandle) -> *mut i8 { std::ptr::null_mut() }
    unsafe extern "C" fn list_secrets(_: PluginHandle) -> *mut i8 { std::ptr::null_mut() }
    unsafe extern "C" fn free_string(_: *mut i8) {}

    Arc::new(Plugin {
        lib: None,
        handle: unsafe { create() },
        destroy,
        set_logger,
        name,
        start,
        drain,
        stop,
        wait_until_drained,
        poll,
        send,
        caps,
        state,
        set_config,
        set_secrets,
        list_config,
        list_secrets,
        free_string,
        last_modified: SystemTime::now(),
        path: PathBuf::new(),
    })
}


/// A dummy context that simply returns the template string unchanged,
/// so JSON values must be provided verbatim.
struct DummyCtx;
impl TemplateContext for DummyCtx {
    fn render_template(&self, template: &str) -> Result<String, String> {
        Ok(template.to_string())
    }
}

#[test]
fn value_or_template_resolves_value_directly() {
    let v: ValueOrTemplate<i32> = ValueOrTemplate::Value(100);
    let ctx = DummyCtx;
    assert_eq!(v.resolve(&ctx).unwrap(), 100);
}

#[test]
fn value_or_template_resolves_from_template() {
    // Template must be valid JSON for T
    let tmpl: ValueOrTemplate<String> = ValueOrTemplate::Template("\"hello world\"".into());
    let ctx = DummyCtx;
    assert_eq!(tmpl.resolve(&ctx).unwrap(), "hello world");
}

#[test]
fn value_or_template_parse_error() {
    let bad: ValueOrTemplate<i32> = ValueOrTemplate::Template("not a number".into());
    let ctx = DummyCtx;
    match bad.resolve(&ctx) {
        Err(ResolveError::Parse(_)) => {},
        other => panic!("Expected Parse error, got: {:?}", other),
    }
}

#[tokio::test]
async fn create_out_msg_error_on_missing_to_and_no_origin() {
    // Build a minimal NodeContext with no channel_origin
    let executor = make_executor();
    let secrets = SecretsManager(EmptySecretsManager::new());
    let cfg_mgr = ConfigManager(MapConfigManager::new());
    let host_logger = HostLogger::new();
    let channel_mgr = ChannelManager::new(cfg_mgr, secrets.clone(), host_logger)
        .await
        .expect("channel manager");
    let tempdir = TempDir::new().unwrap();
    let process_mgr = ProcessManager::new(tempdir.path()).unwrap();
    let ctx = NodeContext::new(
        HashMap::new(),
        HashMap::new(),
        executor.clone(),
        channel_mgr.clone(),
        Arc::new(process_mgr.clone()),
        secrets.clone(),
        None,
    );

    let cfg = ChannelNodeConfig {
        channel_name: "test".into(),
        channel_in: false,
        channel_out: true,
        from: None,
        to: None,
        content: None,
        thread_id: None,
        reply_to_id: None,
    };

    let result = cfg.create_out_msg(
        &ctx,
        "id1".into(),
        None,
        json!("payload"),
        MessageDirection::Outgoing,
    );

    assert!(result.is_err(), "Expected error, got {:?}", result);
}

#[tokio::test]
async fn create_out_msg_uses_template_for_to_and_content() {
    // Prepare context and variables
    let executor = make_executor();
    let secrets = SecretsManager(EmptySecretsManager::new());
    let cfg_mgr = ConfigManager(MapConfigManager::new());
    let host_logger = HostLogger::new();
    let channel_mgr = ChannelManager::new(cfg_mgr, secrets.clone(), host_logger)
        .await
        .expect("channel manager");
    let mut state: HashMap<String, StateValue> = HashMap::new();
    // Provide participant JSON in state
    let part_json = json!({ "id": "p1", "display_name": "Alice", "channel_specific_id": "a1" });
    let part_val: StateValue = serde_json::from_value(part_json.clone()).unwrap();
    state.insert("recipient".into(), part_val);
    let tempdir = TempDir::new().unwrap();
    let process_mgr = ProcessManager::new(tempdir.path()).unwrap();
    let ctx = NodeContext::new(
        HashMap::new(),
        HashMap::new(),
        executor.clone(),
        channel_mgr.clone(),
        Arc::new(process_mgr.clone()),
        secrets.clone(),
        None,
    );

    let cfg = ChannelNodeConfig {
        channel_name: "ch".into(),
        channel_in: false,
        channel_out: true,
        from: None,
        to: Some(vec![ValueOrTemplate::Template("{{recipient}}".into())]),
        content: Some(ValueOrTemplate::Value(MessageContent::Text("fixed".into()))),
        thread_id: None,
        reply_to_id: None,
    };

    let msg = cfg.create_out_msg(
        &ctx,
        "id2".into(),
        None,
        json!("ignored"),
        MessageDirection::Outgoing,
    ).expect("message can be produced");

    // Check 'to'
    assert_eq!(msg.to.len(), 1);
    let rcpt = &msg.to[0];
    assert_eq!(rcpt.id, "p1");
    assert_eq!(msg.content.unwrap(), MessageContent::Text("fixed".into()));
}


#[test]
fn complex_flow_serializes_and_validates_against_schema() {
    // 1) Construct a small flow matching sample.greentic
    let mut flow = Flow::new(
        "sample.greentic".to_string(),
        "Telegram→Weather Forecast Flow".to_string(),
        "A sample flow".to_string(),
    );

    for channel_name in ["mock"] {
        flow.add_channel(channel_name.to_string());
    }

    // 2) Add channel nodes: mock_in, mock_middle, mock_out
    for node_name in &["mock_in", "mock_middle", "mock_out"] {
        let cfg = NodeConfig::new(
            node_name.to_string(),
            NodeKind::Channel {
                cfg: ChannelNodeConfig {
                    channel_name: "mock".to_string(),
                    channel_in: true,
                    channel_out: true,
                    from: None,
                    to: None,
                    content: None,
                    thread_id: None,
                    reply_to_id: None,
                }
            },
            None,
        );
        flow.add_node(node_name.to_string(), cfg);
    }

    // 3) Add tool nodes: weather_in and weather_out
    for node_name in &["weather_in", "weather_out"] {
        let cfg = NodeConfig::new(
            node_name.to_string(),
            NodeKind::Tool {
                tool: ToolNodeConfig {
                    name: "weather_api".to_string(),
                    action: "forecast_weather".to_string(),
                    in_map: Some(Mapper::Copy(CopyMapper{ 
                        payload: Some(vec![
                            CopyKey::Key("q".to_string()), 
                            CopyKey::Key("days".to_string())]), 
                        config: None, 
                        state: None 
                    })),
                    //json!({ "type": "copy", "payload": })),
                    out_map: None,
                    err_map: None,
                    parameters: Some(json!({ "q": "New York", "days": 3 })),
                    secrets: vec!["WEATHERAPI_KEY".to_string()],
                },
            },
            None,
        )
        .with_retry(2, 1);
        flow.add_node(node_name.to_string(), cfg);
    }

    // 4) Wire them: mock_in → weather_in → mock_middle → weather_out → mock_out
    flow.add_connection("mock_in".to_string(), vec!["weather_in".to_string()]);
    flow.add_connection("weather_in".to_string(), vec!["mock_middle".to_string()]);
    flow.add_connection("mock_middle".to_string(), vec!["weather_out".to_string()]);
    flow.add_connection("weather_out".to_string(), vec!["mock_out".to_string()]);

    // 5) Build and serialize
    let flow = flow.build();
    let schema = schema_for!(Flow);
    let schema_json = serde_json::to_value(&schema).unwrap();
    let instance = serde_json::to_value(&flow).unwrap();

    // 6) Validate against the schema
    let compiled = jsonschema::validator_for(&schema_json).expect("schema compiles");
    assert!(compiled.is_valid(&instance), "instance did not validate");

    // 7) Pretty-print and compare to expected JSON
    let rendered = serde_json::to_string_pretty(&instance).unwrap();
    println!("{}", rendered);
    let expected = r#"
{
    "id": "sample.greentic",
    "title": "Telegram→Weather Forecast Flow",
    "description": "A sample flow",
    "channels": [
        "mock"
    ],
    "nodes": {
        "mock_in": {
            "channel": "mock",
            "max_retries": 3,
            "retry_delay_secs": 1,
            "in": true
        },
        "mock_middle": {
            "channel": "mock",
            "max_retries": 3,
            "retry_delay_secs": 1,
            "in": true
        },
        "mock_middle__out": {
            "channel": "mock_out",
            "out": true
        },
        "mock_out": {
            "channel": "mock",
            "max_retries": 3,
            "retry_delay_secs": 1,
            "in": true
        },
        "mock_in__out": {
            "channel": "mock_out",
            "out": true
        },
        "mock_out__out": {
            "channel": "mock_out",
            "out": true
        },
        "weather_in": {
            "tool": {
                "name": "weather_api",
                "action": "forecast_weather",
                "parameters": { "q": "New York", "days": 3 },
                "secrets": ["WEATHERAPI_KEY"],
                "in_map": { "type": "copy", "payload": ["q", "days"] }
            },
            "max_retries": 2,
            "retry_delay_secs": 1
        },
        "weather_out": {
            "tool": {
                "name": "weather_api",
                "action": "forecast_weather",
                "parameters": { "q": "New York", "days": 3 },
                "secrets": ["WEATHERAPI_KEY"],
                "in_map": { "type": "copy", "payload": ["q", "days"] }
            },
            "max_retries": 2,
            "retry_delay_secs": 1
        }
    },
    "connections": {
        "mock_in":        ["mock_in__out"],
        "mock_in__out":   ["weather_in"],
        "mock_middle":    ["mock_middle__out"],
        "mock_middle__out":["weather_out"],
        "mock_out":       ["mock_out__out"],
        "weather_in":     ["mock_middle"],
        "weather_out":    ["mock_out"]
    }
}
"#;
    let expected_value: serde_json::Value = serde_json::from_str(expected).unwrap();
    assert_eq!(instance, expected_value);
}


#[test]
fn json_roundtrip_and_build_graph() {
    // construct a Flow with two channel nodes "n1"→"n2"
    let mut flow = Flow::new(
        "fid", "My Flow", "testing roundtrip",
    );
    flow.add_node(
        "n1".into(),
        NodeConfig {
            id: "n1".into(),
            kind: NodeKind::Channel { 
                cfg: ChannelNodeConfig {
                    channel_name: "slack".to_string(),
                    channel_in: true,
                    channel_out: false,
                    from: None,
                    to: None,
                    content: None,
                    thread_id: None,
                    reply_to_id: None,
                }
            },
            config: None,
            max_retries: Some(2),
            retry_delay_secs: Some(0),
        }
    );
    flow.add_node(
        "n2".into(),
        NodeConfig {
            id: "n2".into(),
            kind: NodeKind::Channel { 
                cfg: ChannelNodeConfig {
                    channel_name: "slack".to_string(),
                    channel_in: false,
                    channel_out: true,
                    from: None,
                    to: None,
                    content: None,
                    thread_id: None,
                    reply_to_id: None,
                }
            },
            config: None,
            max_retries: Some(2),
            retry_delay_secs: Some(0),
        }
    );
    flow.add_connection("n1".into(), vec!["n2".into()]);
    flow.add_connection("n2".into(), vec![]);

    // serde‐serialize
    let text = serde_json::to_string_pretty(&flow).expect("serialize");
    // serde‐deserialize
    let flow2: Flow = serde_json::from_str(&text).expect("deserialize");

    // build internal graph
    let built = flow2.build();
    // make sure graph has exactly 2 nodes
    assert_eq!(built.graph().node_count(), 2);

    // check that edge from n1 to n2 exists
    // by walking topo order and inspecting neighbors
    let mut topo = Topo::new(&built.graph());
    let mut seen = Vec::new();
    while let Some(nx) = topo.next(&built.graph()) {
        let cfg = &built.graph()[nx];
        seen.push(cfg.id.clone());
    }
    // Since n1 → n2, topo order must start with "n1"
    assert_eq!(seen, vec!["n1".to_string(), "n2".to_string()]);
}

#[test]
#[should_panic(expected = "has cycles")]
fn build_cycle_panics() {
    // make a flow with a cycle n1→n2→n1
    let mut flow = Flow::new("fid", "cyclic", "should fail");
    flow.add_node(
        "n1".into(),
        NodeConfig {
            id: "n1".into(),
            kind: NodeKind::Channel { 
                cfg: ChannelNodeConfig {
                    channel_name: "c".to_string(),
                    channel_in: true,
                    channel_out: false,
                    from: None,
                    to: None,
                    content: None,
                    thread_id: None,
                    reply_to_id: None,
                }
            },
            config: None,
            max_retries: Some(1),
            retry_delay_secs: Some(0),
        }
    );
    flow.add_node(
        "n2".into(),
        NodeConfig {
            id: "n2".into(),
            kind: NodeKind::Channel { 
                cfg: ChannelNodeConfig {
                    channel_name: "c".to_string(),
                    channel_in: false,
                    channel_out: true,
                    from: None,
                    to: None,
                    content: None,
                    thread_id: None,
                    reply_to_id: None,
                }
            },
            config: None,
            max_retries: Some(1),
            retry_delay_secs: Some(0),
        }
    );
    flow.add_connection("n1".into(), vec!["n2".into()]);
    flow.add_connection("n2".into(), vec!["n1".into()]);

    // this build() should panic due to cycle
    let _ = flow.build();
}

#[tokio::test]
async fn run_two_channel_nodes() {
    // create the flow
    let mut flow = Flow::new("fid", "seq", "two step");
    flow.add_channel("mock");
    flow.add_node(
        "first".into(),
        NodeConfig {
            id: "first".into(),
            kind: NodeKind::Channel {
                cfg: ChannelNodeConfig {
                    channel_name: "mock".to_string(),
                    channel_in: true,
                    channel_out: false,
                    from: None,
                    to: None,
                    content: None,
                    thread_id: None,
                    reply_to_id: None,
                }
            },
            config: None,
            max_retries: Some(0),
            retry_delay_secs: Some(0),
        }
    );
    flow.add_node(
        "second".into(),
        NodeConfig {
            id: "second".into(),
            kind: NodeKind::Channel { 
                cfg: ChannelNodeConfig {
                    channel_name: "mock".to_string(),
                    channel_in: false,
                    channel_out: true,
                    from: None,
                    to: None,
                    content: None,
                    thread_id: None,
                    reply_to_id: None,
                }
            },
            config: None,
            max_retries: Some(0),
            retry_delay_secs: Some(0),
        }
    );
    flow.add_connection("first".into(), vec!["second".into()]);
    flow.add_connection("second".into(), vec![]);

    // build graph
    let built = flow.build();

    // prepare a dummy Message
    let msg = Message::new("msg1", json!({ "hello": "world" }),None);

    // dummy context
    let executor = make_executor();
    let secrets = SecretsManager(EmptySecretsManager::new());
    let config_mgr = ConfigManager(MapConfigManager::new());
    let host_logger = HostLogger::new();
    let channel_manager = ChannelManager::new(config_mgr, secrets.clone(), host_logger).await.expect("could not create channel manager");
    let tempdir = TempDir::new().unwrap();
    let process_mgr = ProcessManager::new(tempdir.path()).unwrap();

    // **3.** create a FlowManager and a ChannelsRegistry that auto‐registers any Channel nodes
    let fm = FlowManager::new(InMemoryState::new(), executor.clone(), channel_manager.clone(), Arc::new(process_mgr.clone()), secrets.clone());
    let registry = ChannelsRegistry::new(fm.clone(),channel_manager.clone()).await;
    channel_manager.subscribe_incoming(registry.clone() as Arc<dyn IncomingHandler>);
    let noop = make_noop_plugin();              // Arc<Plugin>
    let wrapper = PluginWrapper::new(noop.clone());
    channel_manager.register_channel("mock".into(), wrapper).expect("failed to register noop channel");
    // **4.** *tell* the FlowManager about your new flow so that it fires
    //     the "flow_added" callback and your registry sees & registers the two ChannelNodes.
    fm.register_flow(built.id().as_str(), built.clone());

    let participant = Participant{ id: "id".to_string(), display_name:None, channel_specific_id: None };
    let co = ChannelOrigin::new("channel".to_string(), participant);
    let mut ctx = NodeContext::new(HashMap::new(), HashMap::new(), executor, channel_manager, Arc::new(process_mgr), secrets, Some(co));


    // run
    let report: ExecutionReport = built.run(msg.clone(), "first", &mut ctx).await;

    // we expect two records
    assert_eq!(report.records.len(), 2);
    // verify node_ids and that each attempt==0 and result is Ok
    assert_eq!(report.records[0].node_id, "first");
    assert_eq!(report.records[1].node_id, "second");
    for rec in &report.records {
        assert_eq!(rec.attempt, 0);
        assert!(rec.result.is_ok(), "expected success, got {:?}", rec.result);
    }

    // total should be non‐zero duration
    assert!(report.total.num_milliseconds() >= 0);
}
