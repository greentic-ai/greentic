use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::node::NodeContext;
use crate::message::Message;
use crate::watcher::WatchedType;
use anyhow::Error;
use async_trait::async_trait;
use dashmap::DashMap;


use super::manager::{register_executor, unregister_executor};

pub type ProcessResult = Result<Message, Error>;
/// Each concrete Process backend must register under a unique “type name” here.
/// The `typetag::serde` macro machinery will emit a hidden “type” field in JSON,
/// e.g. `{ "type": "chatgpt", "api_key": "XXX" }`, and choose the correct struct to deserialize.
pub trait ProcessExecutor: Send + Sync {
    /// Returns the unique key under which this executor is registered,
    /// e.g. "chatgpt", "ollama", "deepseek".
    fn process_name(&self) -> &'static str;

    /// Given a `task` string (e.g. "summarize", "classify", "translate"),
    /// an input `Message` payload, and the global `NodeContext`, produce a new
    /// `Message` as the output. Return an `Error` on failure.
    fn execute(
        &self,
        task: &str,
        input: Message,
        ctx: &mut NodeContext,
    ) -> ProcessResult;


    /// Allows cloning a boxed executor. Each implementation must provide this.
    fn clone_box(&self) -> Box<dyn ProcessExecutor>;
}


// Required to let trait‐objects be cloned via `.clone_box()`
impl Clone for Box<dyn ProcessExecutor> {
    fn clone(&self) -> Box<dyn ProcessExecutor> {
        self.clone_box()
    }
}


/// Holds loaded libraries and their process names.
/// We keep the Library alive so symbols remain valid.
#[derive(Clone)]
pub struct ProcessWatcher {
    /// Map from process_name → executor instance
    executors: Arc<DashMap<String, Box<dyn ProcessExecutor>>>,

    /// (Optional) If you also need to know which file corresponds to which name,
    /// keep a second map of PathBuf→String. But if you only need name→executor, skip this.
    path_to_name: Arc<DashMap<PathBuf, String>>,
}

impl ProcessWatcher {
    /// Create a new ProcessWatcher. Typically called once on startup, with an empty map.
    pub fn new() -> Self {
        ProcessWatcher {
            executors: Arc::new(DashMap::new()),
            path_to_name: Arc::new(DashMap::new()),
        }
    }

    /// Return a clone of the internal map (process_name → executor).
    pub fn loaded_executors(&self) -> Arc<DashMap<String, Box<dyn ProcessExecutor>>> {
        Arc::clone(&self.executors)
    }

    /// (Optional) Return the path→name map if you need it elsewhere.
    pub fn path_to_name_map(&self) -> Arc<DashMap<PathBuf, String>> {
        Arc::clone(&self.path_to_name)
    }

    /// Retrieve a cloned executor by its name, if it exists.
    pub fn get_executor(&self, name: &str) -> Option<Box<dyn ProcessExecutor>> {
        self.executors.get(name).map(|entry| entry.value().clone_box())
    }
}
/// Stub for loading a WASM file at `path`
/// and returning `(boxed_executor, process_name_string)`.
/// You must implement actual WASM instantiation here (e.g. via Wasmtime, Wasmer, etc.).
async fn load_wasm_executor(path: &Path) -> anyhow::Result<(Box<dyn ProcessExecutor>, String)> {
    // TODO: 
    //   1) Instantiate the WASM component (e.g. `wasmtime::Instance::new(...)`).
    //   2) Wrap it in a type that implements `ProcessExecutor` (e.g. `WasmProcessExecutor`).
    //   3) Call `executor.process_name()` to get its name.
    //   4) Return `(Box::new(executor), name_string)`.
    Err(anyhow::anyhow!(
        "WASM‐loading not yet implemented for path: {:?}",
        path
    ))
}

/// Stub for unloading/cleaning up a WASM executor, if needed.
/// Often simply dropping the boxed executor is enough, but if you have extra state, do it here.
async fn unload_wasm_executor(_process_name: &str) -> anyhow::Result<()> {
    // TODO: Any special teardown for your WASM runtime.
    Ok(())
}

#[async_trait]
impl WatchedType for ProcessWatcher {
    /// Only watch `.wasm` plugin files.
    fn is_relevant(&self, path: &Path) -> bool {
        matches!(path.extension().and_then(|e| e.to_str()), Some("wasm"))
    }

    /// Called when a plugin is created or modified.
    /// We:
    ///  1) If that path was already loaded, remove the old executor first.
    ///  2) Load the new WASM, register its executor, and remember the name in both maps.
    async fn on_create_or_modify(&self, path: &Path) -> anyhow::Result<()> {
        let pathbuf = path.to_path_buf();

        // 1) If already loaded from this same path, unload the old one
        if let Some((_, old_name)) = self.path_to_name.remove(&pathbuf) {
            // Unregister the old executor
            unregister_executor(&old_name);
            // Remove it from the executors map
            self.executors.remove(&old_name);
            // Perform any WASM teardown
            unload_wasm_executor(&old_name).await?;
        }

        // 2) Attempt to load the WASM and get a boxed executor plus its name
        match load_wasm_executor(path).await {
            Ok((boxed_executor, process_name)) => {
                // 2a) Register the new executor globally
                register_executor(boxed_executor.clone_box());

                // 2b) Insert into our name→executor map
                self.executors.insert(process_name.clone(), boxed_executor);

                // 2c) Remember which path corresponds to which name
                self.path_to_name.insert(pathbuf, process_name.clone());

                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!(
                "Failed to load WASM executor {:?}: {}",
                path,
                e
            )),
        }
    }

    /// Called when a plugin file is removed.
    /// We look up the name, unregister it, and drop it from our maps.
    async fn on_remove(&self, path: &Path) -> anyhow::Result<()> {
        let pathbuf = path.to_path_buf();

        // If this path had been loaded before:
        if let Some((_, process_name)) = self.path_to_name.remove(&pathbuf) {
            // Unregister globally
            unregister_executor(&process_name);

            // Remove from local name→executor map
            self.executors.remove(&process_name);

            // Optionally do WASM teardown
            unload_wasm_executor(&process_name).await?;
        }
        Ok(())
    }
}


// Manually implement Debug since `Box<dyn ProcessExecutor>` doesn’t itself implement Debug.
impl fmt::Debug for ProcessWatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Collect all registered process names:
        let mut names: Vec<String> = Vec::new();
        for entry in self.executors.iter() {
            names.push(entry.key().clone());
        }

        // Collect all loaded paths:
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in self.path_to_name.iter() {
            paths.push(entry.key().clone());
        }

        f.debug_struct("ProcessWatcher")
            .field("registered_names", &names)
            .field("loaded_paths", &paths)
            .finish()
    }
}
