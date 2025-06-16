use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use crate::message::Message;
use crate::node::{NodeContext, NodeErr, NodeError, NodeOut, Routing};
use crate::watcher::WatchedType;
use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::Value;

#[derive(Clone, Debug)]//, Serialize, Deserialize)]
pub struct ProcessInstance {
    pub name: String,
    pub wasm_path: PathBuf,
}

#[derive(Clone, Debug,)]//, Serialize, Deserialize)]
pub struct ProcessWrapper{
    instance: ProcessInstance,
    description: String,
    parameters: Value,
}

impl ProcessWrapper {
    pub fn new(instance: ProcessInstance, description: String, parameters: Value) -> Self {
        Self{instance, description, parameters}
    }

    /// Return the key under which this was registered.
    pub fn name(&self) -> &str {
        &self.instance.name
    }
    
    pub fn description(&self) -> String {
        self.description.clone()
    }

    pub fn instance(&self) -> ProcessInstance {
        self.instance.clone()
    }

    pub fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    pub async fn process(&self, _msg: Message, _ctx: &mut NodeContext) -> Result<NodeOut, NodeErr>
    {
        return Err(NodeErr::with_routing(NodeError::ExecutionFailed(
                        "Not yet implemented".to_string()),
                        Routing::FollowGraph,));
    }
}
/// Holds loaded libraries and their process names.
/// We keep the Library alive so symbols remain valid.
#[derive(Clone)]
pub struct ProcessWatcher {
    /// Map from process_name → executor instance
    processes: Arc<DashMap<String, Box<ProcessWrapper>>>,

    path_to_name: Arc<DashMap<PathBuf, String>>,
}

impl ProcessWatcher {
    /// Create a new ProcessWatcher. Typically called once on startup, with an empty map.
    pub fn new() -> Self {
        ProcessWatcher {
            processes: Arc::new(DashMap::new()),
            path_to_name: Arc::new(DashMap::new()),
        }
    }

    /// Return a clone of the internal map (process_name → executor).
    pub fn loaded_processes(&self) -> Arc<DashMap<String, Box<ProcessWrapper>>> {
        Arc::clone(&self.processes)
    }

    /// Retrieve a cloned executor by its name, if it exists.
    pub fn get_process(&self, name: &str) -> Option<Box<ProcessWrapper>> {
        self.processes.get(name).map(|entry| entry.value().clone())
    }

    pub fn register_process(&self, process: Box<ProcessWrapper>) {
        self.path_to_name.insert(process.instance().wasm_path, process.name().to_string());
        self.processes.insert(process.name().to_string(), process);
    }

    pub fn unregister_process(&self, process: Box<ProcessWrapper>) {
        self.processes.remove(process.name());
        self.path_to_name.remove(&process.instance().wasm_path);
    }

    pub fn unregister_process_via_path(&self, path: PathBuf) {
        if let Some(process_name) = self.path_to_name.get(&path){
            self.processes.remove(process_name.as_str());
        }
        self.path_to_name.remove(&path);
    }
}
/// Stub for loading a WASM file at `path`
/// and returning `(boxed_executor, process_name_string)`.
/// You must implement actual WASM instantiation here (e.g. via Wasmtime, Wasmer, etc.).
async fn load_wasm_executor(path: &Path) -> anyhow::Result<(Box<ProcessWrapper>, String)> {
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
            self.unregister_process_via_path(path.to_path_buf());
            // Remove it from the executors map
            self.processes.remove(&old_name);
            // Perform any WASM teardown
            unload_wasm_executor(&old_name).await?;
        }

        // 2) Attempt to load the WASM and get a boxed executor plus its name
        match load_wasm_executor(path).await {
            Ok((process, process_name)) => {
                // 2a) Register the new executor globally
                self.register_process(process.clone());

                // 2b) Insert into our name→executor map
                self.processes.insert(process_name.clone(), process);

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
            let process = self.get_process(&process_name);
            if let Some(pw) = process {
                self.unregister_process(pw);
            }
            

            // Remove from local name→executor map
            self.processes.remove(&process_name);

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
        for entry in self.processes.iter() {
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
