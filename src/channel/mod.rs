/* 
┌──────────────────────────────────────────────────┐
│                  ChannelManager                 │
│  ┌─────── inbound_rx ──────┐   outbound_tx ──▶ │
│  │  polls plugins FFI     │  sends into FFI    │
│  └─────────────────────────┘                    │
└──────────────────────────────────────────────────┘
                  ▲            ▲
    subscribe     │            │   .send(...)
                  │            │
                  │            │
┌─────────────────┴────────────────┐
│           flow_router.rs        │
│  maps ChannelMessage → Vec<(flow_id,start_node)> 
└─────────────────────────────────┘
                  ▲
                  │ for each (flow, start_node):
                  │
┌─────────────────┴──────────────────┐
│          FlowManager (engine)      │
│   .process_message(flow, node, msg)│
└────────────────────────────────────┘
                  │
     inside flow  ▼
┌──────────────────────────────────┐
│             node.rs             │
│ ┌─ChannelNode─┐  ┌─ToolNode─┐    │
│ │subscribe in │  │ process  │    │
│ │emit out     │  │          │    │
└──────────────────────────────────┘

*/
pub mod manager;
pub mod flow_router;
pub mod node;
pub mod wrapper;
pub mod plugin;

pub use wrapper::PluginWrapper;