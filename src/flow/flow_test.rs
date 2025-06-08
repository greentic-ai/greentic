#[cfg(test)]
mod tests {
    // src/flow_test.rs
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::SystemTime;
    use async_trait::async_trait;
    use channel_plugin::message::{ChannelCapabilities, ChannelMessage, MessageContent, MessageDirection, Participant};
    use channel_plugin::plugin::{ChannelState, PluginLogger};
    use channel_plugin::PluginHandle;
    use schemars::schema::RootSchema;
    use serde::{Deserialize, Serialize};
    use crate::channel::manager::{ChannelManager, HostLogger, IncomingHandler, ManagedChannel};
    use crate::channel::node::ChannelsRegistry;
    use crate::channel::plugin::Plugin;
    use crate::channel::PluginWrapper;
    use crate::config::{ConfigManager, MapConfigManager};
    use crate::mapper::{CopyKey, CopyMapper, Mapper};
    use crate::process::debug_process::DebugProcessNode;
    use crate::process::manager::{BuiltInProcess, ProcessManager};
    use crate::process::script_process::ScriptProcessNode;
    use crate::state::{InMemoryState, StateValue};
    use petgraph::visit::Topo;
    use schemars::{schema_for, JsonSchema};
    use serde_json::json;

    use crate::executor::Executor;
    use crate::flow::manager::{ChannelNodeConfig, ExecutionReport, Flow, FlowManager, NodeConfig, NodeKind, ResolveError, TemplateContext, ToolNodeConfig, ValueOrTemplate};
    use crate::logger::{Logger, OpenTelemetryLogger};
    use crate::message::Message;
    use crate::node::{ChannelOrigin, NodeContext, NodeErr, NodeError, NodeOut, NodeType};
    use crate::secret::{EmptySecretsManager, SecretsManager};
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

    /// little helper to build a NodeContext with no channel_origin
    fn make_ctx() -> NodeContext {
        NodeContext::new(
            HashMap::new(),                     // initial flow‐state
            HashMap::new(),                     // our "local" state
            Executor::dummy(),
            ChannelManager::dummy(),
            ProcessManager::dummy(),
            SecretsManager(EmptySecretsManager::new()),
            None,                               // no channel_origin
        )
    }

    /// A little `ProcessNode` that fails exactly once, then succeeds.
    #[derive(Clone, JsonSchema, Debug, Serialize, Deserialize)]
    struct FailableNode {
        // shared across calls
        #[serde(skip)]
        #[schemars(skip)]
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    #[typetag::serde]
    impl NodeType for FailableNode {
        fn type_name(&self) -> String { "failable".into() }
        fn schema(&self) -> RootSchema { schema_for!(FailableNode) }

        async fn process(&self, msg: Message, _ctx: &mut NodeContext)
            -> Result<NodeOut, crate::node::NodeErr>
        {
            let prev = self.counter.fetch_add(1, Ordering::SeqCst);
            if prev == 0 {
                // first call: fail
                Err(NodeErr::all(NodeError::ExecutionFailed("boom".into())))
            } else {
                // subsequent: echo
                Ok(NodeOut::all(msg))
            }
        }

        fn clone_box(&self) -> Box<dyn crate::node::NodeType> {
            Box::new(self.clone())
        }
    }


    #[tokio::test]
    async fn test_linear_run() {
        // Build a trivial flow: start -> middle -> end
        let mut flow = Flow::new("linear", "Linear", "A → B → C");
        // three debug‐process nodes
        let dbg = BuiltInProcess::Debug(DebugProcessNode{print:false});
        flow.add_node("start".into(), NodeConfig::new(
                "start",
                NodeKind::Process { process: dbg.clone() },
                None
            ));
        flow.add_node("middle".into(), NodeConfig::new(
                "middle",
                NodeKind::Process { process: dbg.clone() },
                None
            ));
        flow.add_node("end".into(), NodeConfig::new(
                "end",
                NodeKind::Process { process: dbg.clone() },
                None
            ));
        flow.add_connection("start".into(), vec!["middle".into()]);
        flow.add_connection("middle".into(), vec!["end".into()]);
        flow.clone().build();

        let mut ctx = make_ctx();
        let msg = Message::new("m1", json!({"foo":"bar"}), None);
        let report = flow.clone().run(msg.clone(), "start", &mut ctx).await;

        // Should have three records, no error:
        assert!(report.error.is_none());
        assert_eq!(report.records.len(), 3);
        // in order: start, middle, end
        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["start","middle","end"]);

        // And each has echoed the same payload
        for rec in report.records {
            assert!(matches!(rec.result, Ok(ref out) if out.message().payload() == msg.payload()));
        }
    }

    #[tokio::test]
    async fn test_branch_and_merge() {
        // A splits to B and C; both feed into D
        //
        //      ┌─> B ┐
        //  A ──┤      ├─> D
        //      └─> C ┘
        //
        // At D we should see payload = [payload_from_B, payload_from_C].

        let mut flow = Flow::new("branch", "Branch & Merge", "");
        let dbg = BuiltInProcess::Debug(DebugProcessNode{print:false});
        flow
            .add_node("A".into(), NodeConfig::new(
                "A", NodeKind::Process { process: dbg.clone() }, None
            ));
        flow.add_node("B".into(), NodeConfig::new(
                "B", NodeKind::Process { process: dbg.clone() }, None
            ));
        flow.add_node("C".into(), NodeConfig::new(
                "C", NodeKind::Process { process: dbg.clone() }, None
            ));
        flow.add_node("D".into(), NodeConfig::new(
                "D", NodeKind::Process { process: dbg.clone() }, None
            ));

        // connections: A→B, A→C; B→D, C→D
        flow.add_connection("A".into(), vec!["B".into(), "C".into()]);
        flow.add_connection("B".into(), vec!["D".into()]);
        flow.add_connection("C".into(), vec!["D".into()]);
        flow.clone().build();

        let mut ctx = make_ctx();
        let input = Message::new("m2", json!({"val":123}), None);
        let report = flow.clone().run(input.clone(), "A", &mut ctx).await;

        // Should have four records (A,B,C,D) in that topo order:
        assert!(report.error.is_none());
        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["A","B","C","D"]);

        // A,B,C each echo the same payload
        for r in &report.records[0..3] {
            assert!(matches!(r.result, Ok(ref o) if o.message().payload() == input.payload()));
        }

        // Now at D we expect an *array* of the two incoming payloads:
        let last = &report.records[3];
        if let Ok(ref out) = last.result {
            let v = out.message().payload();
            // must be Value::Array([{"val":123}, {"val":123}])
            if let serde_json::Value::Array(arr) = v {
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0], json!({"val":123}));
                assert_eq!(arr[1], json!({"val":123}));
            } else {
                panic!("D did not get a merged array, got {:?}", v);
            }
        } else {
            panic!("D failed: {:?}", last.result);
        }
    }

    #[tokio::test]
    async fn test_retry_once() {
        // build flow: start → failable → end
        let mut flow = Flow::new("retry", "Retry", "fail once then succeed");

        // seed the shared counter
        //let failer = FailableNode { counter: Arc::new(AtomicUsize::new(0)) };
        let fp = BuiltInProcess::Script(ScriptProcessNode::new(
            // hack: wrap our FailableNode under ScriptProcessNode so we can inject it
            // but if you have a direct variant you can skip that
            r#"
                if !globalContains("tries") {
                    globalSet("tries", 1);
                    throw "boom";
                } else {
                    payload
                }
            "#.to_string())
        );

        flow.add_node("start".into(), NodeConfig::new("start", NodeKind::Process { process: fp.clone() }, None));
        flow.add_node("end".into(),   NodeConfig::new("end",   NodeKind::Process { process: BuiltInProcess::Debug(DebugProcessNode{print:false}) }, None));
        flow.add_connection("start".into(), vec!["end".into()]);

        // allow 1 retry on our failable node
        flow.nodes().get_mut("start").unwrap().max_retries = Some(1);
        flow.clone().build();

        let mut ctx = make_ctx();
        let input = Message::new("m-retry", json!({"x":1}), None);
        let report = flow.run(input.clone(), "start", &mut ctx).await;

        // we should see two attempts of the "start" node, then one of "end"
        let recs = &report.records;
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].attempt, 0);
        assert!(matches!(recs[0].result, Err(_)));
        assert_eq!(recs[1].attempt, 1);
        assert!(matches!(recs[1].result, Ok(_)));

        // end ran once
        assert_eq!(recs[2].node_id, "end");
        assert!(report.error.is_none());
    }

    #[tokio::test]
    async fn test_out_only_override() {
        // Build A→X and A→Y, but A should override to only Y.
        let mut flow = Flow::new("override", "out_only override", "");
        // A is a tiny process node that always returns NodeOut::one(_, "Y")
        let a_node = BuiltInProcess::Script(ScriptProcessNode::new(
            // hack: wrap our FailableNode under ScriptProcessNode so we can inject it
            // but if you have a direct variant you can skip that
            r#"
                if !globalContains("tries") {
                    globalSet("tries", 1);
                    throw "boom";
                } else {
                    payload
                }
            "#.to_string())
        );

        let dbg = BuiltInProcess::Debug(DebugProcessNode{print:false});
        flow.add_node("A".into(), NodeConfig::new("A", NodeKind::Process { process: a_node }, None));
        flow.add_node("X".into(), NodeConfig::new("X", NodeKind::Process { process: dbg.clone() }, None));
        flow.add_node("Y".into(), NodeConfig::new("Y", NodeKind::Process { process: dbg.clone() }, None));

        // A normally fans to X and Y...
        flow.add_connection("A".into(), vec!["X".into(), "Y".into()]);
        flow.clone().build();

        let mut ctx = make_ctx();
        let input = Message::new("m-o", json!({"ok":true}), None);
        let report = flow.run(input.clone(), "A", &mut ctx).await;

        // records: just A then Y
        assert!(report.error.is_none());
        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["A","Y"]);
    }

    #[tokio::test]
    async fn test_channel_out_node() {
        // simulate a channel‐out node
        let mut flow = Flow::new("chanout", "Channel Out", "");
        let cfg = ChannelNodeConfig {
            channel_name: "mock".into(),
            channel_in:  false,
            channel_out: true,
            from: None,
            to: None,
            content: None,
            thread_id: None,
            reply_to_id: None,
        };
        flow.add_node("chan".into(), NodeConfig::new("chan", NodeKind::Channel { cfg }, None));
        flow.add_node("dbg".into(),   NodeConfig::new("dbg", NodeKind::Process { process: BuiltInProcess::Debug(DebugProcessNode{print:false}) }, None));
        flow.add_connection("chan".into(), vec!["dbg".into()]);
        flow.clone().build();

        // seed a raw Message into "chan"
        let mut ctx = make_ctx();
        let m = Message::new("m-c", json!({"foo":"bar"}), None);
        let report = flow.run(m.clone(), "chan", &mut ctx).await;

        // Should have two records (chan and dbg)
        assert!(report.error.is_none());
        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["chan","dbg"]);

        // The dbg payload should match the original
        assert!(matches!(report.records[1].result, Ok(ref o) if o.message().payload() == m.payload()));
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
            state,
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
            to: Some(vec![ValueOrTemplate::Template("{{recipient.id}}".into())]),
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

        // HOT TO ADD THIS? parameters: Some(json!({ "q": "New York", "days": 3 })),

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
                        on_ok: None,
                        on_err: None,
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
                    "in_map": { "type": "copy", "payload": ["q", "days"] }
                },
                "max_retries": 2,
                "retry_delay_secs": 1
            },
            "weather_out": {
                "tool": {
                    "name": "weather_api",
                    "action": "forecast_weather",
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
        channel_manager.register_channel("mock".into(), ManagedChannel::new(wrapper,None,None)).expect("failed to register noop channel");
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
}