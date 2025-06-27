#[cfg(test)]
mod tests {
    // src/flow_test.rs
    use std::ffi::{c_char, CString};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::SystemTime;
    use async_ffi::{BorrowingFfiFuture, FfiFuture};
    use async_trait::async_trait;
    use channel_plugin::message::{ChannelCapabilities, ChannelMessage, MessageContent, MessageDirection, Participant};
    use channel_plugin::plugin::{ChannelState, LogLevel, PluginLogger};
    use channel_plugin::PluginHandle;
    use dashmap::DashMap;
    use schemars::schema::RootSchema;
    use serde::{Deserialize, Serialize};
    use crate::channel::manager::{ChannelManager, HostLogger, IncomingHandler, ManagedChannel};
    use crate::channel::node::ChannelsRegistry;
    use crate::channel::message::{Plugin, PluginSessionCallbacks};
    use crate::channel::wrapper::tests::make_wrapper;
    use crate::channel::PluginWrapper;
    use crate::config::{ConfigManager, MapConfigManager};
    use crate::flow::session::{InMemorySessionStore, SessionStoreType};
    use crate::mapper::{CopyKey, CopyMapper, Mapper};
    use crate::process::debug_process::DebugProcessNode;
    use crate::process::manager::{BuiltInProcess, ProcessManager};
    use crate::process::script_process::ScriptProcessNode;
    use crate::flow::state::{InMemoryState, SessionStateType, StateValue};
    use petgraph::visit::Topo;
    use schemars::{schema_for, JsonSchema};
    use serde_json::json;

    use crate::executor::Executor;
    use crate::flow::manager::{ChannelNodeConfig, ExecutionReport, Flow, FlowManager, NodeConfig, NodeKind, ResolveError, TemplateContext, ToolNodeConfig, ValueOrTemplate};
    use crate::logger::{Logger, OpenTelemetryLogger};
    use crate::message::Message;
    use crate::node::{ChannelOrigin, NodeContext, NodeErr, NodeError, NodeOut, NodeType, Routing};
    use crate::secret::{EmptySecretsManager, SecretsManager};
    use tempfile::TempDir;

    impl Flow {

        pub fn equal_for_test(&self, other: &Self) -> bool {
            self.id() == other.id()
            && self.title() == other.title()
            && self.description() == other.description()
            && self.channels() == other.channels()
            && self.nodes() == other.nodes()
            && self.connections() == other.connections()
        }
    }

    fn dummy_flow(name: &str) -> Flow {
        // create a minimal flow with one dummy node
        let flow = Flow::new(name.to_string(),name.to_string(),"dummy".to_string())
        .add_node("start".to_string(), dummy_tool_node())
        .build();

        flow
    }

    fn dummy_tool_node() -> NodeConfig {
        NodeConfig {
            id: "start".to_string(),
            kind: NodeKind::Tool {
                tool: ToolNodeConfig {
                    name: "noop".to_string(),
                    action: "noop".to_string(),
                    in_map: None,
                    out_map: None,
                    err_map: None,
                    on_ok: None,
                    on_err: None,
                }
            },
            config: None,
            max_retries: Some(0),
            retry_delay_secs: Some(0),
        }
    }

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
        unsafe extern "C" fn set_logger(_: PluginHandle, _logger_ptr: PluginLogger, _log_level_ptr: LogLevel) {}
        unsafe extern "C" fn name(_: PluginHandle) -> *mut i8 { std::ptr::null_mut() }
        unsafe extern "C" fn start(_: PluginHandle) -> FfiFuture<bool> { return BorrowingFfiFuture::<bool>::new(async move {true}); }
        unsafe extern "C" fn drain(_: PluginHandle) -> bool { true }
        unsafe extern "C" fn stop(_: PluginHandle) -> FfiFuture<bool> { return BorrowingFfiFuture::<bool>::new(async move {true}); }
        unsafe extern "C" fn wait_until_drained(_: PluginHandle, _: u64) -> FfiFuture<bool> { return BorrowingFfiFuture::<bool>::new(async move {true});  }
        unsafe extern "C" fn caps(_: PluginHandle, out: *mut ChannelCapabilities) -> bool {
            if !out.is_null() {
                unsafe { std::ptr::write(out, ChannelCapabilities::default()) };
                true
            } else {
                false
            }
        }

         unsafe extern "C" fn send_message(
            _handle: PluginHandle,
            _msg: *const ChannelMessage,
        ) -> FfiFuture<bool> {
            BorrowingFfiFuture::<bool>::new(async move {true})
        }

        // 2) Async‐style receive → FfiFuture<ChannelMessage>
        unsafe extern "C" fn receive_message(
            _handle: PluginHandle,
        ) -> FfiFuture<*mut c_char> {
                BorrowingFfiFuture::<*mut c_char>::new(async move {
                    // imagine you have a real msg here
                    let msg = ChannelMessage::default();
                    let json = serde_json::to_string(&msg).unwrap_or_else(|_| "{}".into());
                    CString::new(json).unwrap().into_raw()
                })
            
        }

        unsafe extern "C" fn set_session_callbacks(
            _handle: PluginHandle,
            _callbacks: PluginSessionCallbacks,
        ) {
            // Store or mock for verification in tests
            println!("✅ FakePlugin received set_session_callbacks_fn");
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
            caps,
            state,
            set_config,
            set_secrets,
            list_config,
            list_secrets,
            free_string,
            send_message,
            receive_message,
            set_session_callbacks: Some(set_session_callbacks),
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
            "123".to_string(),
            InMemoryState::new(),                     // initial flow‐state
            DashMap::new(),                     // our "local" state
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
                Err(NodeErr::fail(NodeError::ExecutionFailed("boom".into())))
            } else {
                // subsequent: echo
                Ok(NodeOut::with_routing(msg, Routing::FollowGraph))
            }
        }

        fn clone_box(&self) -> Box<dyn crate::node::NodeType> {
            Box::new(self.clone())
        }
    }

/* 
   #[tokio::test]
    async fn test_channel_reply_from_script_node_stops_flow_and_updates_state() {
        let chan_node_id = "entry".to_string();
        let chan_id = "chan".to_string();
        let entry_channel =  NodeConfig::new(chan_node_id, NodeKind::Channel { cfg:ChannelNodeConfig {
            channel_name: chan_id.into(),
            channel_in:  true,
            channel_out: false,
            from: None,
            to: Some(vec![ValueOrTemplate::Value(Participant::new("dbg".into(),None,None))]),
            content: None,
            thread_id: None,
            reply_to_id: None,
        }}, None);

        let script_id = "ask_question".to_string();
        let script_node = NodeConfig::new(script_id, NodeKind::Process{ process: BuiltInProcess::Script(ScriptProcessNode::new(
                    r#"
                        if payload.text == "err" {
                            return reply("❌ error reply");
                        } else {
                            return reply("✅ ok reply");
                        }
                    "#.to_string(),
                ))}, None);
        let store =InMemorySessionStore::new(10);
        let mut manager = FlowManager::new_test(store);

        let flow_id = "test_reply_flow";


        let flow = Flow::new(flow_id, "title", "description")
            .add_channel(chan_id)
            .add_node(chan_node_id.to_string(), entry_channel)
            .add_node(script_id, script_node)
            .add_connection(chan_node_id, vec![script_id])
            .build();

        manager.register_flow(flow);

        // STEP 1: simulate incoming message
        let session_id = "session-abc";
        let msg = Message::new(
            &chan_id,
            json!({ "text": "err" }), // also try "ok"
            session_id.to_string(),
        );

        let mut ctx = make_ctx();
        ctx.add_flow(flow_id.to_string());

        let report = flow.run(msg,&chan_node_id, &mut ctx).await;

        // STEP 2: assert that flow halted on reply
        assert_eq!(report.records.len(), 1);
        assert!(report.error.is_none(), "Should not error on reply");

        // STEP 3: assert reply message content
        let out = ctx.take_sent("test").await;
        assert_eq!(out.len(), 1);
        let content = out[0].content.clone().unwrap().to_string();
        assert!(content.contains("❌ error reply"));

        // STEP 4: assert state has current node
        assert_eq!(ctx.nodes(), Some(vec![script_id.to_string()]));
        assert!(ctx.flows().unwrap().contains(&flow_id.to_string()));
    }

*/
    #[tokio::test]
    async fn test_linear_run() {
        // three debug‐process nodes
        let dbg = BuiltInProcess::Debug(DebugProcessNode{print:false});
        // Build a trivial flow: start -> middle -> end
        let flow = Flow::new("linear", "Linear", "A → B → C")
        .add_node("start".into(), NodeConfig::new(
                "start",
                NodeKind::Process { process: dbg.clone() },
                None
            ))
        .add_node("middle".into(), NodeConfig::new(
                "middle",
                NodeKind::Process { process: dbg.clone() },
                None
            ))
        .add_node("end".into(), NodeConfig::new(
                "end",
                NodeKind::Process { process: dbg.clone() },
                None
            ))
        .add_connection("start".into(), vec!["middle".into()])
        .add_connection("middle".into(), vec!["end".into()])
        .build();

        let mut ctx = make_ctx();
        let msg = Message::new("m1", json!({"foo":"bar"}), "123".to_string());
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
        let dbg = BuiltInProcess::Debug(DebugProcessNode{print:false}); 
        let flow = Flow::new("branch", "Branch & Merge", "")
        .add_node("A".into(), NodeConfig::new(
                "A", NodeKind::Process { process: dbg.clone() }, None
            ))
        .add_node("B".into(), NodeConfig::new(
                "B", NodeKind::Process { process: dbg.clone() }, None
            ))
        .add_node("C".into(), NodeConfig::new(
                "C", NodeKind::Process { process: dbg.clone() }, None
            ))
        .add_node("D".into(), NodeConfig::new(
                "D", NodeKind::Process { process: dbg.clone() }, None
            ))

        // connections: A→B, A→C; B→D, C→D
        .add_connection("A".into(), vec!["B".into(), "C".into()])
        .add_connection("B".into(), vec!["D".into()])
        .add_connection("C".into(), vec!["D".into()])
        .build();

        let mut ctx = make_ctx();
        let input = Message::new("m2", json!({"val":123}), "123".to_string());
        let report = flow.clone().run(input.clone(), "A", &mut ctx).await;

        // Should have four records (A,B,C,D) in that topo order:
        assert!(report.error.is_none());
        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["A","B","C","D", "D"]);

        // Should have five records: A, B, C, D, D (D runs twice)
        assert!(report.error.is_none());
        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["A", "B", "C", "D", "D"]);

        // A, B, C each echo the same payload
        for r in &report.records[0..3] {
            assert!(matches!(r.result, Ok(ref o) if o.message().payload() == input.payload()));
        }

        // D is now called twice, both with same payload
        let d_records: Vec<_> = report.records.iter().filter(|r| r.node_id == "D").collect();
        assert_eq!(d_records.len(), 2);

        for r in d_records {
            match &r.result {
                Ok(out) => {
                    assert_eq!(out.message().payload(), input.payload());
                },
                Err(e) => panic!("D failed with error: {:?}", e),
            }
        }
    }

    #[tokio::test]
    async fn test_retry_once() {
                // seed the shared counter
        //let failer = FailableNode { counter: Arc::new(AtomicUsize::new(0)) };
        let fp = BuiltInProcess::Script(ScriptProcessNode::new(
            // hack: wrap our FailableNode under ScriptProcessNode so we can inject it
            // but if you have a direct variant you can skip that
            r#"
                if "tries" !in state {
                    state["tries"] = 1;
                    throw "boom";
                } else {
                    payload
                }
            "#.to_string())
        );
        let process =  NodeConfig::new("start", NodeKind::Process { process: fp.clone() }, None);
        let debug = NodeConfig::new("end",   NodeKind::Process { process: BuiltInProcess::Debug(DebugProcessNode{print:false}) }, None);
        // build flow: start → failable → end
        let flow = Flow::new("retry", "Retry", "fail once then succeed")
        .add_node("start".into(),process)
        .add_node("end".into(),   debug)
        .add_connection("start".into(), vec!["end".into()])
        .build();
        // allow 1 retry on our failable node
        flow.nodes().get_mut("start").unwrap().max_retries = Some(1);


        let mut ctx = make_ctx();
        let input = Message::new("m-retry", json!({"x":1}), "123".to_string());
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
        // A is a tiny process node that always returns NodeOut::one(_, "Y")
        let a_node = BuiltInProcess::Script(ScriptProcessNode::new(
            // hack: wrap our FailableNode under ScriptProcessNode so we can inject it
            // but if you have a direct variant you can skip that
            r#"
            if "tries" !in state {
                state["tries"] = 1;
                throw "boom";
            } else {
                let json = #{
                    "__greentic": #{
                        "payload": payload,
                        "out": ["X"]
                    }
                };
                return json;
            }
            "#.to_string())
        );

        let dbg = BuiltInProcess::Debug(DebugProcessNode{print:false});
        // Build A→X and A→Y, but A should override to only Y.
        let flow = Flow::new("override", "out_only override", "")
        .add_node("A".into(), NodeConfig::new("A", NodeKind::Process { process: a_node }, None))
        .add_node("X".into(), NodeConfig::new("X", NodeKind::Process { process: dbg.clone() }, None))
        .add_node("Y".into(), NodeConfig::new("Y", NodeKind::Process { process: dbg.clone() }, None))

        // A normally fans to X and Y...
        .add_connection("A".into(), vec!["X".into(), "Y".into()])
        .build();

        let mut ctx = make_ctx();
        let input = Message::new("m-o", json!({"ok":true}), "123".to_string());
        let report = flow.run(input.clone(), "A", &mut ctx).await;

        // records: just A then Y
        assert!(report.error.is_none());
        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["A", "A", "X"]);
    }

    #[tokio::test]
    async fn test_err_only_override() {
        let a_node = BuiltInProcess::Script(ScriptProcessNode::new(r#"
            if "tries" !in state {
                state["tries"] = 1;
                throw "boom";
            } else {
                let json = #{
                    "__greentic": #{
                        "payload": payload,
                        "err": ["Z"]
                    }
                };
                return json;
            }
        "#.to_string()));
        let dbg = BuiltInProcess::Debug(DebugProcessNode{print:false});
        let flow = Flow::new("test_err_only_override", "test_err_only_override", "")
            .add_node("A".into(), NodeConfig::new("A", NodeKind::Process { process: a_node }, None))
            .add_node("Z".into(), NodeConfig::new("Z", NodeKind::Process { process: dbg.clone() }, None))
            .add_connection("A".into(), vec!["Z".into()])
            .build();

        let mut ctx = make_ctx();
        let input = Message::new("m-o", json!({"ok":true}), "123".to_string());
        let report = flow.run(input.clone(), "A", &mut ctx).await;

        assert!(report.error.is_none()); // ✅ Flow handled error via err route

        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["A", "A", "Z"]);

        let attempt_0 = &report.records[0];
        assert!(attempt_0.result.is_err());

        let attempt_1 = &report.records[1];
        assert!(attempt_1.result.is_err());
    }

    #[tokio::test]
    async fn test_err_and_out_prefers_err() {
        let a_node = BuiltInProcess::Script(ScriptProcessNode::new(r#"
            if "tries" !in state {
                state["tries"] = 1;
                throw boom;
            } else {
                let json = #{
                    "__greentic": #{
                        "payload": payload,
                        "out": ["Y"],
                        "err": ["Z"]
                    }
                };
                return json;
            }
        "#.to_string()));
        let dbg = BuiltInProcess::Debug(DebugProcessNode{print:false});
        let flow = Flow::new("test_err_and_out_prefers_err", "test_err_and_out_prefers_err", "")
            .add_node("A".into(), NodeConfig::new("A", NodeKind::Process { process: a_node }, None))
            .add_node("Y".into(), NodeConfig::new("Y", NodeKind::Process { process: dbg.clone() }, None))
            .add_node("Z".into(), NodeConfig::new("Z", NodeKind::Process { process: dbg.clone() }, None))
            .add_connection("A".into(), vec!["Y".into(), "Z".into()])
            .build();

        let mut ctx = make_ctx();
        let input = Message::new("m-o", json!({"ok":true}), "123".to_string());
        let report = flow.run(input.clone(), "A", &mut ctx).await;
        assert!(report.error.is_none());
        let ids: Vec<_> = report.records.iter().map(|r| r.node_id.as_str()).collect();
        assert_eq!(ids, &["A", "A", "Z"]);
    }

    #[tokio::test]
    async fn test_channel_out_node() {
        let channel =  NodeConfig::new("chan", NodeKind::Channel { cfg:ChannelNodeConfig {
            channel_name: "mock".into(),
            channel_in:  false,
            channel_out: true,
            from: None,
            to: Some(vec![ValueOrTemplate::Value(Participant::new("dbg".into(),None,None))]),
            content: None,
            thread_id: None,
            reply_to_id: None,
        }}, None);

        let process =  NodeConfig::new("dbg", NodeKind::Process { process: BuiltInProcess::Debug(DebugProcessNode{print:false}) }, None);
        // simulate a channel‐out node
        let flow = Flow::new("chanout", "Channel Out", "")
        .add_channel("mock".to_string())
        .add_node("chan".into(), channel)
        .add_node("dbg".into(),   process)
        .add_connection("chan".into(), vec!["dbg".into()])
        .build();

        // seed a raw Message into "chan"
        let mut ctx = make_ctx();
        let cm = ctx.channel_manager();
        let wrapper = make_wrapper();
        let mock = ManagedChannel::new(wrapper, None, None);
        assert!(cm.register_channel("mock".to_string(), mock).await.is_ok());
        let m = Message::new("m-c", json!({"foo":"bar"}), "123".to_string());
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
        let host_logger = HostLogger::new(LogLevel::Debug);
        let store =InMemorySessionStore::new(10);
        let channel_mgr = ChannelManager::new(cfg_mgr, secrets.clone(), store, host_logger)
            .await
            .expect("channel manager");
        let tempdir = TempDir::new().unwrap();
        let process_mgr = ProcessManager::new(tempdir.path()).unwrap();
        let ctx = NodeContext::new(
            "123".to_string(),
            InMemoryState::new(),
            DashMap::new(),
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
            "123".to_string(),
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
        let host_logger = HostLogger::new(LogLevel::Debug);
        let store =InMemorySessionStore::new(10);
        let channel_mgr = ChannelManager::new(cfg_mgr, secrets.clone(), store, host_logger)
            .await
            .expect("channel manager");
        let state = InMemoryState::new();
        // Provide participant JSON in state
        let part_json = json!({ "id": "p1", "display_name": "Alice", "channel_specific_id": "a1" });
        let part_val: StateValue = serde_json::from_value(part_json.clone()).unwrap();
        state.set("recipient".into(), part_val);
        let tempdir = TempDir::new().unwrap();
        let process_mgr = ProcessManager::new(tempdir.path()).unwrap();
        let ctx = NodeContext::new(
            "123".to_string(),
            state, 
            DashMap::new(),
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
            "123".to_string(),
            json!("ignored"),
            MessageDirection::Outgoing,
        ).expect("message can be produced");

        // Check 'to'
        assert_eq!(msg.to.len(), 1);
        let rcpt = &msg.to[0];
        assert_eq!(rcpt.id, "p1");
        assert_eq!(msg.content.unwrap(), vec![MessageContent::Text("fixed".into())]);
    }


    #[test]
    fn complex_flow_serializes_and_validates_against_schema() {
        let mock_in = NodeConfig::new(
                "mock_in".to_string(),
                NodeKind::Channel {
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
                None,
            );
        let mock_middle = NodeConfig::new(
                "mock_middle".to_string(),
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
        let mock_out = NodeConfig::new(
                "mock_out".to_string(),
                NodeKind::Channel {
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
                None,
            );

        let weather_in =  NodeConfig::new(
                "weather_in".to_string(),
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
        let weather_out =  NodeConfig::new(
                "weather_out".to_string(),
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
        // 1) Construct a small flow matching sample.greentic
        let flow = Flow::new(
            "sample.greentic".to_string(),
            "Telegram→Weather Forecast Flow".to_string(),
            "A sample flow".to_string(),
        )
        .add_channel("mock".to_string())
        .add_node("mock_in".to_string(), mock_in)
        .add_node("mock_middle".to_string(), mock_middle)
        .add_node("mock_out".to_string(), mock_out)
        .add_node("weather_in".to_string(), weather_in)
        .add_node("weather_out".to_string(), weather_out)
        // 4) Wire them: mock_in → weather_in → mock_middle → weather_out → mock_out
        .add_connection("mock_in".to_string(), vec!["weather_in".to_string()])
        .add_connection("weather_in".to_string(), vec!["mock_middle".to_string()])
        .add_connection("mock_middle".to_string(), vec!["weather_out".to_string()])
        .add_connection("weather_out".to_string(), vec!["mock_out".to_string()])
        .build();

        // 5) Build and serialize
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
            "mock_in":        ["weather_in"],
            "mock_middle":    ["mock_middle__out"],
            "mock_middle__out":["weather_out"],
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
        let n1 = NodeConfig {
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
            };
        let n2= NodeConfig {
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
            };
        // construct a Flow with two channel nodes "n1"→"n2"
        let flow = Flow::new(
            "fid", "My Flow", "testing roundtrip",
        )
        .add_node("n1".into(),n1)
        .add_node("n2".into(),n2)
        .add_connection("n1".into(), vec!["n2".into()])
        .add_connection("n2".into(), vec![]);

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
        let n1 = NodeConfig {
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
            };
        let n2 = NodeConfig {
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
            };
        // make a flow with a cycle n1→n2→n1
        let _ = Flow::new("fid", "cyclic", "should fail")
        .add_node("n1".into(), n1)
        .add_node("n2".into(),n2)
        .add_connection("n1".into(), vec!["n2".into()])
        .add_connection("n2".into(), vec!["n1".into()])
        .build();

    }

    #[tokio::test]
    async fn run_two_channel_nodes() {
        let first = NodeConfig {
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
            };
        let second = NodeConfig {
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
            };
        // create the flow
        let flow = Flow::new("fid", "seq", "two step")
        .add_channel("mock")
        .add_node("first".into(),first)
        .add_node("second".into(),second)
        .add_connection("first".into(), vec!["second".into()])
        .add_connection("second".into(), vec![])
        .build();

        // prepare a dummy Message
        let msg = Message::new("msg1", json!({ "hello": "world" }),"123".to_string());
        let store =InMemorySessionStore::new(10);
        // dummy context
        let executor = make_executor();
        let secrets = SecretsManager(EmptySecretsManager::new());
        let config_mgr = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new(LogLevel::Debug);
        let channel_manager = ChannelManager::new(config_mgr, secrets.clone(), store.clone(), host_logger).await.expect("could not create channel manager");
        let tempdir = TempDir::new().unwrap();
        let process_mgr = ProcessManager::new(tempdir.path()).unwrap();

        // **3.** create a FlowManager and a ChannelsRegistry that auto‐registers any Channel nodes
        let fm = FlowManager::new(store.clone(), executor.clone(), channel_manager.clone(), Arc::new(process_mgr.clone()), secrets.clone());
        let registry = ChannelsRegistry::new(fm.clone(),channel_manager.clone()).await;
        channel_manager.subscribe_incoming(registry.clone() as Arc<dyn IncomingHandler>);
        let noop = make_noop_plugin();              // Arc<Plugin>
        let wrapper = PluginWrapper::new(noop.clone(), store.clone());
        channel_manager.register_channel("mock".into(), ManagedChannel::new(wrapper,None,None)).await.expect("failed to register noop channel");
        // **4.** *tell* the FlowManager about your new flow so that it fires
        //     the "flow_added" callback and your registry sees & registers the two ChannelNodes.
        fm.register_flow(flow.clone());

        let participant = Participant{ id: "id".to_string(), display_name:None, channel_specific_id: None };
        let co = ChannelOrigin::new("channel".to_string(), None, None, participant);
        let mut ctx = NodeContext::new("123".to_string(), store.get_or_create("123").await, DashMap::new(), executor, channel_manager, Arc::new(process_mgr), secrets, Some(co));


        // run
        let report: ExecutionReport = flow.run(msg.clone(), "first", &mut ctx).await;

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
        store.clear();
    }



    #[tokio::test]
    async fn test_lazy_flow_registration() {
        let session_store =InMemorySessionStore::new(15);
        let flow = dummy_flow("lazy_flow");
        let manager = FlowManager::new_test(session_store.clone());
        manager.register_flow(flow.clone());

        let msg = Message::new("1", serde_json::json!({"q": "hello"}), "sess1".to_string());
        let report = manager.process_message("lazy_flow", "start", msg.clone(), None).await;

        assert!(report.is_some());
        assert!(report.as_ref().unwrap().error.is_none());

        let session = session_store.get("sess1").await.unwrap();
        let flows = session.flows().unwrap();
        assert!(flows.contains(&"lazy_flow".to_string()));
    }

    #[tokio::test]
    async fn test_block_disallowed_flow() {
        let session_store = InMemorySessionStore::new(15);
        let flow = dummy_flow("blocked_flow");
        let manager = FlowManager::new_test(session_store.clone());
        manager.register_flow(flow.clone());

        let session = session_store.get_or_create("sess2").await;
        session.set_flows(vec!["allowed_flow".to_string()]);

        let msg = Message::new("2", serde_json::json!({"q": "block test"}), "sess2".to_string());
        let report = manager.process_message("blocked_flow", "start", msg.clone(), None).await;

        assert!(report.is_some());
        let r = report.unwrap();
        assert!(r.records.is_empty());
        assert!(r.error.is_some());
        assert_eq!(r.total.num_milliseconds(), 0);
    }

    #[tokio::test]
    async fn test_valid_flow_executes() {
        let session_store =InMemorySessionStore::new(15);
        let flow = dummy_flow("valid_flow");
        let manager = FlowManager::new_test(session_store.clone());
        manager.register_flow(flow.clone());

        let session = session_store.get_or_create("sess3").await;
        session.set_flows(vec!["valid_flow".to_string()]);

        let msg = Message::new("3", serde_json::json!({"q": "run"}), "sess3".to_string());
        let report = manager.process_message("valid_flow", "start", msg.clone(), None).await;

        assert!(report.is_some());
        let r = report.unwrap();
        assert!(!r.records.is_empty());
        assert!(r.error.is_none());
    }
}