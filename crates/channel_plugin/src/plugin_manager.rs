// src/actor_manager.rs  (≈ 60 LOC)

use std::path::Path;

use crate::{jsonrpc::{self, Request}, plugin_actor::{spawn_process_actor, ActorHandle, PluginEvent}};       // same run() you wrote
use dashmap::DashMap;
use tokio::sync::{mpsc::{self, unbounded_channel, UnboundedReceiver, UnboundedSender}};

#[derive(Debug, Clone)]
pub struct ActorManager {
    actors: DashMap<String, ActorHandle>,
    events: mpsc::UnboundedSender<PluginEvent>,
}

impl ActorManager {
    pub fn new() -> (Self, UnboundedReceiver<(std::string::String, jsonrpc::Request)>){
        let (tx, rx) = unbounded_channel::<(String, Request)>();
        (
            Self {
                actors: DashMap::new(),
                events: tx,
            },
            rx,
        )
    }

        /// Anyone can subscribe for `PluginEvent`s.
    pub fn event_sender(&self) -> UnboundedSender<PluginEvent> {
        self.events.clone()
    }


/// Spawn a plugin process, wire it up to a new actor.
    pub async fn spawn_plugin<P: AsRef<Path>>(&mut self, id: &str, exe_path: P) -> anyhow::Result<()> {
       let handle = spawn_process_actor(exe_path, self.events.clone()).await?; 
        if self.actors.insert(id.to_string(), handle).is_some() {
            anyhow::bail!("plugin `{id}` already exists");
        }
        Ok(())
    }

    /// Send a **notification** (no response expected).
    pub async fn send_notification(
        &self,
        id: &str,
        req: jsonrpc::Request,
    ) -> anyhow::Result<()> {
        let h = self
            .actors
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown plugin"))?;
        let _ = h.call(req).await;
        Ok(())
    }

    /// Send a **call** and wait for the JSON-RPC response.
    pub async fn send_request(
        &self,
        id: &str,
        req: jsonrpc::Request,
    ) -> anyhow::Result<jsonrpc::Response> {
        let h = self
            .actors
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown plugin"))?;
        h.call(req).await
    }

}
