//! ProcessManager: holds exactly the baked-in variants and also watches a directory
//! for any filesystem plugins (“.wasm” files).  As plugins appear, they get registered
//! under the registry by their name; as they disappear, they get unregistered.

use crate::message::Message;
use crate::node::{NodeContext, NodeErr, NodeError, NodeOut};
use crate::process::qa_process::QAProcessNode;
use crate::watcher::DirectoryWatcher;
use crate::{node::NodeType, process::process::ProcessWatcher};
use anyhow::Error;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::error;

// You must have these types already defined elsewhere, each implementing `ProcessWrapper + JsonSchema + Serialize/Deserialize`.
use super::{
    debug_process::DebugProcessNode, script_process::ScriptProcessNode,
    template_process::TemplateProcessNode,
};

/// An enum of all built-in variants plus a “Plugin” marker for any external WASM on disk.
///
/// When you load a `.wasm` plugin from `/plugins/foo.wasm`, you will spawn a
/// `BuiltInProcess::Plugin { name: "foo".into(), path: "/plugins/foo.wasm".into() }`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BuiltInProcess {
    /// A baked-in “debug” process
    Debug(DebugProcessNode),

    /// A baked-in script process (for example, a Rhia script)
    Script(ScriptProcessNode),

    /// A baked-in Handlebars template process
    Template(TemplateProcessNode),

    /// A Question & Answer process with optional agent support
    #[serde(rename = "qa")]
    Qa(QAProcessNode),

    /// A disk-loaded plugin (e.g. `/plugins/foo.wasm` → name = "foo").
    Plugin {
        /// Unique name of the plugin (used as registry key)
        name: String,

        /// On-disk path (we skip serializing this)
        #[serde(skip)]
        path: PathBuf,
    },
}

impl BuiltInProcess {
    /// Return the key under which this should be registered.
    /// - For a baked-in variant, forward to its `process_name()`.
    /// - For `Plugin { name, .. }`, return that `name`.
    pub fn process_name(&self) -> String {
        match self {
            BuiltInProcess::Debug(inner) => inner.type_name(),
            BuiltInProcess::Script(inner) => inner.type_name(),
            BuiltInProcess::Template(inner) => inner.type_name(),
            BuiltInProcess::Qa(inner) => inner.type_name(),
            BuiltInProcess::Plugin { name, .. } => name.clone(),
        }
    }

    pub async fn process(&self, msg: Message, ctx: &mut NodeContext) -> Result<NodeOut, NodeErr> {
        match self {
            BuiltInProcess::Debug(inner) => inner.process(msg, ctx).await,
            BuiltInProcess::Script(inner) => inner.process(msg, ctx).await,
            BuiltInProcess::Template(inner) => inner.process(msg, ctx).await,
            BuiltInProcess::Qa(inner) => inner.process(msg, ctx).await,
            BuiltInProcess::Plugin { name, .. } => {
                let watcher = ctx.process_manager().clone().watcher;
                if watcher.is_none() {
                    let error = format!("ProcessWatcher should be set in process manager");
                    error!(error);
                    return Err(NodeErr::fail(NodeError::Internal(error)));
                }
                let process = watcher.unwrap().get_process(name);
                match process {
                    Some(process) => process.process(msg, ctx).await,
                    None => {
                        let error = format!("Process {} was not found", name);
                        error!(error);
                        return Err(NodeErr::fail(NodeError::NotFound(error)));
                    }
                }
            }
        }
    }

    /// If this is a `Plugin { path, .. }`, return `Some(&path)`.  Otherwise `None`.
    pub fn plugin_path(&self) -> Option<&PathBuf> {
        match self {
            BuiltInProcess::Plugin { path, .. } => Some(path),
            _ => None,
        }
    }
}

/// Manages exactly the baked-in variants plus any `.wasm` plugins under `process_dir`.
#[derive(Debug, Clone)]
pub struct ProcessManager {
    /// Filesystem directory where we watch for `*.wasm` plugins.
    pub process_dir: PathBuf,

    /// If Some, this watcher will keep track of “on_create_or_modify / on_remove”
    pub watcher: Option<ProcessWatcher>,
}

impl ProcessManager {
    /// Construct a new `ProcessManager`.
    ///
    /// - `built_ins`: e.g. `vec![ BuiltInProcess::Debug(DebugProcessNode::default()), … ]`
    /// - `process_dir`: directory to watch for external `.wasm` plugins
    pub fn new(process_dir: impl Into<PathBuf>) -> Result<Self, anyhow::Error> {
        let dir = process_dir.into();
        // 1) Ensure the directory exists on disk
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
        }

        Ok(ProcessManager {
            process_dir: dir,
            watcher: None,
        })
    }

    /// Start watching `self.process_dir` for new/modified/removed `.wasm` files.
    /// Each time a `.wasm` appears, the `ProcessWatcher` should load it (e.g. via WASM components),
    /// wrap it in a `Box<ProcessWrapper>`, and then call `register_process(...)`.
    /// On removal, it will call `unregister_process(...)`.
    ///
    /// Returns the spawned task handle.  You can keep it if you want to `await` or abort later.
    pub async fn watch_process_dir(&mut self) -> Result<DirectoryWatcher, Error> {
        // Create a new watcher (initially empty).  It knows how to load/unload WASM Plugin executors.
        let watcher = ProcessWatcher::new();
        self.watcher = Some(watcher.clone());

        // Kick off the actual filesystem watcher:
        // The `&["wasm"]` means “only watch files ending in `.wasm`.”
        let watch_path = self.process_dir.clone();
        Ok(DirectoryWatcher::new(
            watch_path.clone(),
            Arc::new(watcher.clone()),
            &["wasm"],
            true,
        )
        .await?)
    }

    /// Unregister all dynamic plugins (but leave baked-ins alone).
    /// After this, the `PROCESS_REGISTRY` will only contain the baked-ins again.
    pub fn shutdown_plugins(&self) {
        if let Some(watcher) = &self.watcher {
            // Walk every entry in the global registry.  If it’s not one of our baked-in names,
            // remove it:
            let to_remove: Vec<String> = watcher
                .loaded_processes()
                .iter()
                .map(|entry| entry.key().clone())
                .collect();
            for name in to_remove {
                let process = watcher.get_process(&name);
                if process.is_some() {
                    watcher.unregister_process(process.unwrap());
                }
            }
        }
    }
}
#[cfg(test)]
pub mod tests {
    use crate::{
        process::process::{ProcessInstance, ProcessWrapper},
        watcher::WatchedType,
    };

    use super::*;
    use anyhow::Result;
    use std::path::PathBuf;

    // ------------------------------------------------------------------------------------------------
    // 1) We want a minimal “dummy” ProcessWrapper that satisfies the interface expected by
    //    ProcessWatcher / ProcessManager.  In your real code, `ProcessWrapper` likely has
    //    a constructor, `instance() -> &ProcessInstance`, and `name() -> &str`.  Here we fake one.
    // ------------------------------------------------------------------------------------------------

    impl ProcessWrapper {
        pub fn dummy<P: Into<PathBuf>, S: Into<String>>(path: P, name: S) -> Box<ProcessWrapper> {
            let instance = ProcessInstance {
                wasm_path: path.into(),
                name: name.into(),
            };
            let wrapper =
                ProcessWrapper::new(instance.into(), "dummy".into(), serde_json::Value::Null);
            Box::new(wrapper)
        }
    }

    impl ProcessManager {
        pub fn dummy() -> Arc<ProcessManager> {
            Arc::new(Self {
                process_dir: PathBuf::new(),
                watcher: None,
            })
        }
    }

    // We assume that `ProcessWrapper` has these two methods:
    //   fn instance(&self) -> &ProcessInstance
    //   fn name(&self) -> &str
    //
    // (If your real `ProcessWrapper` is named slightly differently, adjust accordingly.)

    // ------------------------------------------------------------------------------------------------
    // 2) Test that `ProcessManager::new(...)` creates the directory and leaves `watcher == None`.
    // ------------------------------------------------------------------------------------------------

    #[tokio::test]
    async fn new_process_manager_creates_dir_and_has_no_watcher() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let dir_path = temp_dir.path().join("plugins");
        // Ensure “plugins” does not yet exist
        assert!(!dir_path.exists());

        // Create a new manager; it must create the directory on disk
        let manager = ProcessManager::new(&dir_path).unwrap();
        assert!(
            manager.process_dir.exists(),
            "Expected directory to be created"
        );
        assert!(
            manager.watcher.is_none(),
            "Watcher should start out as None"
        );

        Ok(())
    }

    // ------------------------------------------------------------------------------------------------
    // 3) Test `watch_process_dir(...)` spawns a task and sets `watcher = Some(...)`.
    //    We cannot actually trigger a filesystem event without more infrastructure,
    //    but we can at least ensure the return‐type is a JoinHandle and watcher is set.
    // ------------------------------------------------------------------------------------------------

    #[tokio::test]
    async fn watch_process_dir_sets_watcher() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let dir_path = temp_dir.path().join("plugins");
        std::fs::create_dir_all(&dir_path)?;

        let mut manager = ProcessManager::new(&dir_path).unwrap();

        // Initially no watcher
        assert!(manager.watcher.is_none());

        // Spawn the watcher task
        let handle = manager.watch_process_dir().await?;
        // After calling, watcher should be Some(...)
        assert!(manager.watcher.is_some());
        handle.shutdown();

        Ok(())
    }

    // ------------------------------------------------------------------------------------------------
    // 4) Test `ProcessWatcher` on its own: insert a dummy `ProcessWrapper` into the maps, then
    //    call `on_create_or_modify` and `on_remove` directly.  We expect the wrapper to move
    //    into `loaded_processes()` on create, and to vanish on remove.  (All via interior mutability.)
    // ------------------------------------------------------------------------------------------------

    #[tokio::test]
    async fn process_watcher_create_and_remove() -> Result<()> {
        let watcher = ProcessWatcher::new();

        // Initially, no processes are loaded
        assert!(watcher.loaded_processes().iter().next().is_none());

        // 4a) Simulate loading a WASM plugin
        //     We define a dummy path and manually register a Box<ProcessWrapper>.
        let dummy_path = PathBuf::from("/tmp/fake_plugin.wasm");
        let dummy_name = "fake_plugin";

        // Step-by-step: load a “DummyProcessWrapper” and insert it
        let wrapper = ProcessWrapper::dummy(&dummy_path, dummy_name);
        // Normally, on_create_or_modify would do this insertion after loading WASM.
        watcher.register_process(wrapper.clone());

        // Now we should see exactly one entry in `loaded_processes()`
        let mut found = false;
        for entry in watcher.loaded_processes().iter() {
            assert_eq!(entry.key(), dummy_name);
            found = true;
        }
        assert!(found, "Expected the dummy wrapper to be present");

        // 4b) Simulate removal of the same path
        watcher.on_remove(&dummy_path).await?;

        // After removal, there should be no entry with that name
        assert!(
            watcher.get_process(dummy_name).is_none(),
            "Expected the dummy wrapper to be gone"
        );

        Ok(())
    }

    // ------------------------------------------------------------------------------------------------
    // 5) Test `ProcessManager::shutdown_plugins()`: if the watcher holds a dummy, shutdown removes it
    // ------------------------------------------------------------------------------------------------

    #[tokio::test]
    async fn shutdown_plugins_removes_dynamic_entries() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let dir_path = temp_dir.path().join("plugins");
        std::fs::create_dir_all(&dir_path)?;

        // 5a) Create manager and attach a fresh watcher
        let mut manager = ProcessManager::new(&dir_path).unwrap();
        let watcher = ProcessWatcher::new();
        manager.watcher = Some(watcher.clone());

        // 5b) Manually register two dummy wrappers under the watcher
        let path1 = dir_path.join("one.wasm");
        let name1 = "one";
        let wrapper1 = ProcessWrapper::dummy(&path1, name1);
        watcher.register_process(wrapper1.clone());

        let path2 = dir_path.join("two.wasm");
        let name2 = "two";
        let wrapper2 = ProcessWrapper::dummy(&path2, name2);
        watcher.register_process(wrapper2.clone());

        // Sanity‐check: both are present
        assert!(watcher.get_process(name1).is_some());
        assert!(watcher.get_process(name2).is_some());

        // 5c) Now call `shutdown_plugins()`.  That should iterate over watcher.loaded_processes()
        //      and remove each dynamic wrapper from the watcher itself.
        manager.shutdown_plugins();

        // After shutdown, neither “one” nor “two” remain
        assert!(watcher.get_process(name1).is_none());
        assert!(watcher.get_process(name2).is_none());

        Ok(())
    }

    // ------------------------------------------------------------------------------------------------
    // 6) Test round‐trip: when a file is modified twice in a row, the old wrapper is unloaded
    //    and replaced by a new one under the same name.  We simulate via two calls to on_create_or_modify.
    // ------------------------------------------------------------------------------------------------

    #[tokio::test]
    async fn create_modify_replaces_old_wrapper() -> Result<()> {
        let watcher = ProcessWatcher::new();
        let path = PathBuf::from("/tmp/reload.wasm");
        let name = "reloadable";

        // First load
        let wrapper_v1 = ProcessWrapper::dummy(&path, name);
        watcher.register_process(wrapper_v1.clone());

        // Sanity: loaded
        assert!(watcher.get_process(name).is_some());

        // Now simulate “file changed” by manually calling `on_create_or_modify`
        // But our stub load_wasm_executor is unimplemented.  Instead, we replicate
        // what on_create_or_modify would do if it succeeded twice:
        //   - remove old
        watcher.unregister_process(wrapper_v1);
        //   - insert new
        let wrapper_v2 = ProcessWrapper::dummy(&path, name);
        watcher.register_process(wrapper_v2.clone());

        // Final: only v2 remains (we can't inspect v1 vs v2 directly, but only one entry)
        let mut count = 0;
        for entry in watcher.loaded_processes().iter() {
            assert_eq!(entry.key(), name);
            count += 1;
        }
        assert_eq!(count, 1, "Expected exactly one entry under that name");

        Ok(())
    }
}
