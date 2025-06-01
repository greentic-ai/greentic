//! ProcessManager: registers built-in processes and manages dynamic processes via an ProcessWatcher.
use crate::process::process::ProcessWatcher;
use crate::watcher::watch_dir;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Error;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use tokio::task::JoinHandle;
use super::process::ProcessExecutor;

/// ProcessManager holds built-in process names and a watcher for dynamic (WASM) processes.
#[derive(Debug, Clone)]
pub struct ProcessManager {
    /// Names of all built-in processes (strings from process_name()).
    built_in: HashSet<String>,
    /// Path to the plugin directory being watched.
    process_dir: PathBuf,
    /// The watcher that handles on-create/modify/remove.
    watcher: Option<ProcessWatcher>,
}

impl ProcessManager {
    /// Create a new ProcessManager.
    ///
    /// - `built_ins`: a list of boxed ProcessExecutor implementations to register immediately.
    /// - `process_dir`: path to watch for dynamically loaded process plugins (.wasm files).
    ///
    /// This will register all built-in executors into the global registry, then spawn
    /// a watcher task that monitors `process_dir`.  When new process plugins appear or are removed,
    /// the watcher registers/unregisters them accordingly.
    pub async fn new(
        built_ins: Vec<Box<dyn ProcessExecutor>>,
        process_dir: impl Into<PathBuf>,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let mut built_set = HashSet::new();
        // Register each built-in
        for exec in built_ins {
            let name = exec.process_name().to_string();
            register_executor(exec);
            built_set.insert(name);
        }

        let plugin_path: PathBuf = process_dir.into();
        // Ensure plugin directory exists
        if !plugin_path.exists() {
            std::fs::create_dir_all(&plugin_path)?;
        }

        Ok(Arc::new(ProcessManager {
            built_in: built_set,
            process_dir: plugin_path,
            watcher:None,
        }))
    }

    pub async fn watch_process_dir(&mut self, process_dir: PathBuf) -> Result<JoinHandle<()>,Error> {
                // Initialize the ProcessWatcher
        let watcher = ProcessWatcher::new();
        self.watcher = Some(watcher.clone());
        // Start watching; true = recursive
        let handle = watch_dir(process_dir.clone(), Arc::new(watcher.clone()), &["wasm"], true).await;
   
        handle
    }

    /// Return a list of all registered process names, both built‐in
    /// and any dynamic (WASM‐loaded) executors.
    pub fn list_processes(&self) -> Vec<String> {
        // 1) Start with a clone of the built‐in set
        let mut names: HashSet<String> = self.built_in.clone();

        // 2) If there's a watcher, iterate over its DashMap of executors
        if let Some(watcher) = &self.watcher {
            // `loaded_executors()` returns `Arc<DashMap<String, Box<dyn ProcessExecutor>>>`
            let executors = watcher.loaded_executors();
            let loaded_map: &DashMap<String, Box<dyn ProcessExecutor>> = executors.as_ref();

            // For each entry, `.key()` is `&String` (the process_name)
            for entry in loaded_map.iter() {
                names.insert(entry.key().clone());
            }
        }

        // 3) Collect into a Vec<String> and return
        names.into_iter().collect()
    }

    /// Stop watching the plugin directory and unregister all dynamic processes.
    pub fn shutdown(&self) {
        if let Some(watcher) = &self.watcher {
            // Same as above: get `&DashMap<PathBuf, (Library, String)>`
            let loaded_map_arc:&Arc<DashMap<String, Box<dyn ProcessExecutor + 'static>>>   = &watcher.loaded_executors();
            let loaded_map:&DashMap<String, Box<dyn ProcessExecutor + 'static>> = loaded_map_arc.as_ref();

            // For each entry, the value’s `.1` is the process name
            for entry in loaded_map.iter() {
                let process_name:&'static str = entry.value().process_name();
                unregister_executor(process_name);
            }
        }
    }

    /// Manually register a new built-in executor at runtime.
    pub fn register_builtin(&mut self, exec: Box<dyn ProcessExecutor>) {
        let name = exec.process_name().to_string();
        register_executor(exec);
        self.built_in.insert(name);
    }

    /// Manually unregister a built-in executor (it will remain in built_in set until restart).
    /// Note: Unregistering built-ins at runtime may lead to flows losing their executor.
    pub fn unregister_builtin(&mut self, name: &str) {
        unregister_executor(name);
        self.built_in.remove(name);
    }
}


// Global, thread-safe registry mapping process names to boxed executors.
static PROCESS_REGISTRY: Lazy<DashMap<String, Box<dyn ProcessExecutor>>> =
    Lazy::new(|| DashMap::new());

/// Registers a new executor under its `process_name()`. If an executor with the same
/// name already exists, it will be replaced.
///
/// # Arguments
///
/// * `executor` - A boxed object implementing `ProcessExecutor`.
pub fn register_executor(executor: Box<dyn ProcessExecutor>) {
    let name = executor.process_name().to_string();
    PROCESS_REGISTRY.insert(name, executor);
}

/// Unregisters (removes) an executor by its name. If no executor with that name
/// is found, this function does nothing.
///
/// # Arguments
///
/// * `process_name` - The string name under which the executor was registered.
pub fn unregister_executor(process_name: &str) {
    PROCESS_REGISTRY.remove(process_name);
}

/// Looks up an executor by name. Returns `Some(Box<dyn ProcessExecutor>)` if found,
/// or `None` otherwise. Clones the boxed executor so the caller owns a fresh instance.
///
/// # Arguments
///
/// * `process_name` - The string key used when registering the executor.
///
/// # Returns
///
/// * `Option<Box<dyn ProcessExecutor>>` - A cloned boxed executor if found.
pub fn get_executor(process_name: &str) -> Option<Box<dyn ProcessExecutor>> {
    PROCESS_REGISTRY
        .get(process_name)
        .map(|entry| entry.value().clone_box())
}

#[cfg(test)]
mod tests {
    use super::*;


    /// A trivial `ProcessExecutor` implementation for testing.
    #[derive(Clone)]
    struct TestProcess {
        name: &'static str,
    }

    impl TestProcess {
        fn new(name: &'static str) -> Self {
            TestProcess { name }
        }
    }

    impl ProcessExecutor for TestProcess {
        fn process_name(&self) -> &'static str {
            self.name
        }

        fn execute(
            &self,
            _task: &str,
            input: crate::message::Message,
            _ctx: &mut crate::node::NodeContext,
        ) -> Result<crate::message::Message, anyhow::Error> {
            // Just echo the input  payload back as-is:
            Ok(input)
        }

        fn clone_box(&self) -> Box<dyn ProcessExecutor> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn register_and_get_executor() {
        // Ensure clean registry for the test
        unregister_executor("testprocess");

        let process = Box::new(TestProcess::new("testprocess"));
        register_executor(process);

        // Now we should be able to `get_executor("testprocess")`
        let maybe = get_executor("testprocess");
        assert!(maybe.is_some());
        let retrieved = maybe.unwrap();
        assert_eq!(retrieved.process_name(), "testprocess");

        // Unregister and verify it is gone
        unregister_executor("testprocess");
        assert!(get_executor("testprocess").is_none());
    }

    #[test]
    fn get_executor_returns_none_for_missing() {
        unregister_executor("nonexistent");
        assert!(get_executor("nonexistent").is_none());
    }
}