use std::fs;
use std::sync::Arc;
use std::path::{Path, PathBuf};
use async_trait::async_trait;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{debug, error};
use wasmtime_wasi::p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView,};
use std::time::SystemTime;
use anyhow::{bail, Context, Error, Result};
use dashmap::DashMap;
use exports::wasix::mcp::router::{CallToolResult, Tool, ToolError, Value as McpValue};
use exports::wasix::mcp::secrets_list::SecretsDescription;
use serde_json::Value;
use wasi::logging::logging;
use wasix::mcp::secrets_store::{Host, HostSecret, Secret, SecretValue, SecretsError, add_to_linker};
use wasmtime::{Engine, Store};
use wasmtime::component::{Component, Linker, Resource};
use wasmtime_wasi::{ResourceTable};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpView};
use futures_util::FutureExt;
use crate::secret::{EmptySecretsManager, SecretsManager};
use crate::watcher::{watch_dir, WatchedType};
use std::fmt::Debug;
use crate::logger::{LogLevel, Logger};

// Import the configuration type from your config module.

// Import the MCP component interface using the wasmtime bindgen macro.

wasmtime::component::bindgen!({
    world: "mcp-secrets",
    //additional_derives: [serde::Serialize, serde::Deserialize],
});


/// Our custom state for WASI.
struct MyState {
    secrets_manager: SecretsManager,
    #[allow(unused)]
    logging: Logger,
    table: ResourceTable,
    ctx: WasiCtx,
    http: WasiHttpCtx,
}

impl HostSecret for MyState {
    fn drop(&mut self, _rep: wasmtime::component::Resource<Secret>) -> wasmtime::Result<()> {
        self.secrets_manager.0 = EmptySecretsManager::new(); // Or however you create a fresh one
        Ok(())
    }
}

impl IoView for MyState {
    fn table(&mut self) -> &mut ResourceTable { &mut self.table }
}
impl WasiView for MyState {
    fn ctx(&mut self) -> &mut WasiCtx { &mut self.ctx }
}

impl WasiHttpView for MyState {
    fn ctx(&mut self) -> &mut WasiHttpCtx {
        &mut self.http
    }
}

impl Host for MyState {
    fn get(&mut self, key: String) -> Result<Resource<Secret>, SecretsError> {
        match self.secrets_manager.0.get(&key) {
            Some(id) => Ok(Resource::new_own(id)),
            None => Err(SecretsError::NotFound),
        }
    }

    fn reveal(&mut self, handle: Resource<Secret>) -> SecretValue {
        if let Some(result) = self.secrets_manager.0.reveal(handle.rep()).now_or_never() {
            match result {
                Ok(Some(secret)) => SecretValue { secret },
                Ok(None) | Err(_) => SecretValue { secret: "".into() },
            }
        } else {
            SecretValue { secret: "".into() }
        }
    }
}

impl logging::Host for MyState {
    fn log(&mut self,level:logging::Level,context:wasmtime::component::__internal::String,message:wasmtime::component::__internal::String,) -> () {
        match level {
            logging::Level::Trace => self.logging.0.log(LogLevel::Trace, &context, &message),
            logging::Level::Debug => self.logging.0.log(LogLevel::Debug, &context, &message),
            logging::Level::Info => self.logging.0.log(LogLevel::Info, &context, &message),
            logging::Level::Warn => self.logging.0.log(LogLevel::Warn, &context, &message),
            logging::Level::Error => self.logging.0.log(LogLevel::Error, &context, &message),
            logging::Level::Critical => self.logging.0.log(LogLevel::Critical, &context, &message),
        }
    }
}
/* 
impl Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretValue").field("secret", &"*******").finish()
    }
}
*/

/// A tool instance holding the instantiated MCP router.
#[derive(Clone, Debug)]//, Serialize, Deserialize)]
pub struct ToolInstance {
    pub tool_id: String,
    pub wasm_path: PathBuf,
    pub secrets_list: Vec<SecretsDescription>,
    pub tools_list: Vec<Tool>,
}


/// The Executor loads and instantiates tools from the tools dir.
#[derive(Clone, Debug)]//, Serialize, Deserialize)]
pub struct Executor {
    /// Mapping from tool key (as defined in config.tools) to an instantiated ToolInstance.
    tools: Arc<DashMap<String, Arc<ToolWrapper>>>,
    secrets_manager: SecretsManager,
    logger: Logger,
}

impl Executor {
    pub fn new(secrets_manager: SecretsManager, logger: Logger) -> Arc<Self> {
        Arc::new(Executor {
            tools: Arc::new(DashMap::new()),
            secrets_manager,
            logger,
        })
    }

    pub async fn watch_tool_dir(&self, tool_dir: PathBuf) -> Result<JoinHandle<()>,Error>  {
        let handler = ToolDirHandler {
            tools: Arc::clone(&self.tools),
            secrets: self.secrets_manager.clone(),
            logging: self.logger.clone(),
        };
        // watch_dir will do exactly the same setup+loop+pattern-matching you already wrote for channels:
        watch_dir(tool_dir, Arc::new(handler), &["wasm"], true).await
    }


    
    pub fn get_tool(&self, tool_name: String) -> Option<Arc<ToolWrapper>> {
        self.tools.get(&tool_name).map(|entry| Arc::clone(entry.value()))
    }

    #[tracing::instrument(name = "call_tool", skip(self))]
    pub fn call_tool(&self, tool_name: String, tool_action: String, tool_args:  Value) -> Result<CallToolResult, ToolError> {
        let tool_key = format!("{}_{}", tool_name, tool_action);
        debug!("Tools loaded: {}",self.tools.len());
        match self.tools.get(&tool_key) 
        {
            Some(tool) => {
                match convert_value(tool_args.clone()) {
                    
                    Ok(tool_val) => {
                        let mut config = wasmtime::Config::default();
                        config.async_support(false);

                        // Create a Wasmtime engine and store
                        let engine = Engine::new(&config).unwrap();

                        // Build a WASI context.
                        let wasi_ctx = WasiCtxBuilder::new().build();
                        let state = MyState {
                            secrets_manager: self.secrets_manager.clone(),
                            logging: self.logger.clone(),
                            ctx: wasi_ctx,
                            http: WasiHttpCtx::new(),
                            table: ResourceTable::new(),
                        };
                        let mut store = Store::new(&engine, state);
                        // Load the wasm component from file.
                        let component = Component::from_file(&engine, &tool.tool_instance.wasm_path)
                            .with_context(|| format!("Failed to load wasm component from {:?}", &tool.tool_instance.wasm_path)).unwrap();


                        // Create a linker and add WASI support.
                        let mut linker = Linker::new(&engine);
                        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
                            .context("Failed to add WASI to linker").unwrap();
                        wasmtime_wasi_http::add_only_http_to_linker_sync(&mut linker).expect("Could not add http to linker");
                        add_to_linker(&mut linker,  |state: &mut MyState| state).expect("Could not link secrets store");
                        logging::add_to_linker(&mut linker, |state: &mut MyState| state).expect("Could not link logging");
                        // Instantiate the MCP component.
                        let router = McpSecrets::instantiate(&mut store, &component, &linker)
                            .with_context(|| format!("Failed to instantiate MCP component for tool '{}'", &tool.tool_instance.tool_id)).expect("Could not instantiate");
                        tokio::task::block_in_place(|| {
                            router.wasix_mcp_router().call_call_tool(&mut store, tool_action.as_str(), &tool_val).expect("Could not call tool")
                        })
                    }
                    _ => Err(ToolError::InvalidParameters(format!("Could not call {} in {} because the tool args {} does not have a jey.",tool_action, tool_name, tool_args)))
                }
            },
            _ => {
                let error = format!("Could not call {} in {} because the tool does not exist.",tool_action, tool_name);
                error!(error);
                Err(ToolError::NotFound(error))
            }
        }

    }

    pub fn list_tool_keys(&self) -> Vec<String> {
        self.tools.iter().map(|entry| entry.key().clone()).collect()
    }
}

struct ToolDirHandler {
  tools: Arc<DashMap<String,Arc<ToolWrapper>>>,
  secrets: SecretsManager,
  logging: Logger,
}


#[async_trait]
impl WatchedType for ToolDirHandler {
  fn is_relevant(&self, path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("wasm")
  }

  async fn on_create_or_modify(&self, path: &Path) -> anyhow::Result<()> {
    reload_with_retry(&path.to_path_buf(), Arc::clone(&self.tools), self.secrets.clone(), self.logging.clone()).await;
    Ok(())
  }

  async fn on_remove(&self, path: &Path) -> anyhow::Result<()> {
    if let Some(tool_id) = path.file_stem().and_then(|s| s.to_str()) {
      self.tools.retain(|k, _| !k.starts_with(&format!("{}_", tool_id)));
    }
    Ok(())
  }
}

 #[tracing::instrument(name = "load_tool", skip(tools_map, secrets_manager,logging))]
async fn reload_tool_from_path(
    tools_map: &Arc<DashMap<String, Arc<ToolWrapper>>>,
    secrets_manager: SecretsManager,
    logging: Logger,
    path: PathBuf
) -> Result<()> {
    wait_until_file_is_stable(&path, Duration::from_millis(300), Duration::from_secs(5)).await?;
    // Extract tool ID from filename
    let tool_id = path.file_stem().unwrap().to_str().unwrap().to_string();
    let instance = instantiate_tool(secrets_manager.clone(), logging.clone(), &tool_id, path.clone())?;
    for tool in &instance.tools_list {
        let name = format!("{}_{}", instance.tool_id, tool.name);
        let params: Value = serde_json::from_str(&tool.input_schema.json)?;
        let wrapper = ToolWrapper::new(instance.clone(), tool.name.clone(), tool.description.clone(), instance.secrets_list.clone(), params);
        tools_map.insert(name, Arc::new(wrapper));
    }
    Ok(())
}

async fn reload_with_retry(path: &PathBuf, tools_ref: Arc<DashMap<String, Arc<ToolWrapper>>>, secrets: SecretsManager, logging: Logger) {
    const MAX_RETRIES: usize = 10;

    for attempt in 0..MAX_RETRIES {
        match reload_tool_from_path(&tools_ref, secrets.clone(), logging.clone(), path.clone()).await {
            Ok(_) => return,
            Err(e) => {
                if attempt == MAX_RETRIES - 1 {
                    tracing::error!("Failed to load {:?} after retries: {:?}", path, e);
                } else {
                    tracing::warn!("Retrying load for {:?} (attempt {}): {:?}", path, attempt + 1, e);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}


#[derive(Clone, Debug,)]//, Serialize, Deserialize)]
pub struct ToolWrapper{
    tool_instance: ToolInstance,
    tool_method: String,
    description: String,
    secrets: Vec<SecretsDescription>,
    parameters: Value,
}

impl ToolWrapper {
    pub fn new(tool_instance: ToolInstance, tool_method: String, description: String, secrets: Vec<SecretsDescription>, parameters: Value) -> Self {
        Self{tool_instance, tool_method, description, secrets, parameters}
    }

    pub fn name(&self) -> String {
        format!("{}_{}", self.tool_instance.tool_id,self.tool_method)
    }

    pub fn tool_id(&self) -> String {
        self.tool_instance.tool_id.clone()
    }

    pub fn tool_method_id(&self) -> String {
        self.tool_method.clone()
    }

    pub fn description(&self) -> String {
        self.description.clone()
    }

    pub fn secrets(&self) -> Vec<SecretsDescription> {
        self.secrets.clone()
    }

    pub fn parameters(&self) -> Value {
        self.parameters.clone()
    }
}

/// Converts a serde_json::Value into an McpValue.
/// Expects the input to be a JSON object with exactly one key-value pair.
pub fn convert_value(tool_args: Value) -> Result<McpValue> {
    match tool_args.is_null() {
        false => {
            Ok(McpValue { json: tool_args.to_string() })
        }
        _ => bail!("Expected a JSON object"),
    }
}

/// Instantiates a tool given its tool_id, the full path to its wasm file, and a Wasmtime engine.
/// It creates a WASI context, a store, and uses a linker to instantiate the component.
/// The resulting MCP router is returned wrapped in a ToolInstance.
fn instantiate_tool(secrets_manager: SecretsManager, logging: Logger, wasm_file: &str, wasm_path: PathBuf) -> Result<ToolInstance> {
    let mut config = wasmtime::Config::default();
    config.async_support(false);

    // Create a Wasmtime engine and store
    let engine = Engine::new(&config).unwrap();

    // Build a WASI context.
    let wasi_ctx = WasiCtxBuilder::new().build();
    let state = MyState {
        secrets_manager,
        logging,
        ctx: wasi_ctx,
        http: WasiHttpCtx::new(),
        table: ResourceTable::new(),
    };
    let mut store = Store::new(&engine, state);
    // Load the wasm component from file.
    let component = Component::from_file(&engine, &wasm_path)
        .with_context(|| format!("Failed to load wasm component from {:?}", &wasm_path))?;
    // Create a linker and add WASI support.
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
        .context("Failed to add WASI to linker")?;
    wasmtime_wasi_http::add_only_http_to_linker_sync(&mut linker).expect("Could not add http to linker");
    add_to_linker(&mut linker,  |state: &mut MyState| state).expect("Could not link secrets store");
    logging::add_to_linker(&mut linker, |state: &mut MyState| state).expect("Could not link logging");

    // Instantiate the MCP component.
    let router = McpSecrets::instantiate(&mut store, &component, &linker)
        .with_context(|| format!("Failed to instantiate MCP component for tool '{}'", wasm_file)).unwrap();

    let tool_name = router.wasix_mcp_router().call_name(&mut store).unwrap();
    let secrets_list = router.wasix_mcp_secrets_list().call_list_secrets(&mut store).unwrap();
    let tools_list = router.wasix_mcp_router().call_list_tools(&mut store).unwrap();

    // (Optional) Here you could list the required secrets via the mcp-secrets interface
    // and load them from environment variables if needed.

    Ok(ToolInstance {
        tool_id: tool_name,
        wasm_path,
        secrets_list,
        tools_list,
    })
}

/// Wait until file is stable (unchanged) for `stable_for` duration.
/// Fails if it doesn't stabilize within `timeout`.
pub async fn wait_until_file_is_stable(path: &Path, stable_for: Duration, timeout: Duration) -> std::io::Result<()> {
    let mut last_size = 0;
    let mut last_mtime = SystemTime::UNIX_EPOCH;
    let mut stable_elapsed = Duration::ZERO;
    let mut total_elapsed = Duration::ZERO;

    let interval = Duration::from_millis(100);

    while total_elapsed < timeout {
        match fs::metadata(path) {
            Ok(meta) => {
                let size = meta.len();
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

                if size == last_size && mtime == last_mtime {
                    stable_elapsed += interval;
                    if stable_elapsed >= stable_for {
                        return Ok(());
                    }
                } else {
                    stable_elapsed = Duration::ZERO;
                    last_size = size;
                    last_mtime = mtime;
                }
            }
            Err(e) => return Err(e),
        }

        sleep(interval).await;
        total_elapsed += interval;
    }

    Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "File never stabilized"))
}

#[cfg(test)]
mod tests {
    use tokio::time::timeout;

    use super::*;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;
    use crate::executor::exports::wasix::mcp::router;
    use crate::logger::OpenTelemetryLogger;
    use crate::secret::{EmptySecretsManager, EnvSecretsManager};

    #[tokio::test(flavor = "multi_thread")]
    async fn test_dynamic_tool_watcher_load_and_remove() {
        let tool_dir = Path::new("./tests/wasm/tools_load_remove").to_path_buf();
        let test_wasm = tool_dir.join("weather_api.wasm");
        // remove the test file in case the previous test failed
        if test_wasm.exists() {
            fs::remove_file(test_wasm.clone()).expect("could not remove test wasm");
        }

        let secrets_manager = SecretsManager(EmptySecretsManager::new());
        let logging = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets_manager, logging);

        // Spawn the watcher in the background
        let executor_clone = Arc::clone(&executor);
        tokio::spawn(async move {
            executor_clone.watch_tool_dir(tool_dir.clone()).await.expect("could not start watcher");
        });

        // Wait briefly to ensure the watcher is running
        tokio::time::sleep(Duration::from_millis(300)).await;

        // copy the weather wasm
        let weather_wasm = Path::new("./tests/wasm/tools_call/weather_api.wasm");
        fs::copy(weather_wasm, test_wasm.clone()).expect("could not copy weather wasm");           

        // Wait for the watcher to find the file. Watcher has been configured for 2 seconds
        tokio::time::sleep(Duration::from_millis(3000)).await;

        // At least one tool should now be loaded
        let keys = executor.list_tool_keys();
        for key in keys.clone() {
            print!("{} ",key);
        }
        assert!(keys.iter().any(|k| k.starts_with("weather_api_forecast_weather")), "Expected tool not loaded");

        // Remove the file
        try_remove_file_until_gone(&test_wasm, 20);
        assert!(wait_until_removed(&test_wasm, 5000).await, "WASM file was not removed in time");

        // needed for the executor to notice the file is gone and to update the tools list
        tokio::time::sleep(Duration::from_millis(2000)).await;

        let tool_keys = executor.list_tool_keys();
        assert!(tool_keys.iter().all(|k| !k.starts_with("weather_api_forecast_weather")), "Tool was not removed");
        
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_call_weather_tool() {

        let test_wasm = Path::new("./tests/wasm/tools_call/weather_api.wasm");
        assert!(test_wasm.exists(), "WASM file should exist before running this test");

        let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(PathBuf::from("./greentic/secrets"))));
        let logging = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets_manager, logging);

        reload_tool_from_path(&executor.tools, executor.secrets_manager.clone(), executor.logger.clone(), test_wasm.to_path_buf()).await.expect("should load tool");

        let input = serde_json::json!({ "q": "London", "days": 1,  });
       let result = 
            executor.call_tool("weather_api".into(), "forecast_weather".into(), input);
        match result {
            Ok(CallToolResult { content, is_error }) => {
                if is_error == Some(true) {
                    panic!("Error in result: {:?}",content);
                } else if let Some(router::Content::Text(text)) = content.get(0) {
                    let reply = text.text.as_str();
                    let val: Value = serde_json::from_str(reply).unwrap();
                    assert!(val.get("current").and_then(|c| c.get("temp_c")).is_some(), "should return temp_c in current");
                } else {
                    panic!("Expected first content to be Text");
                }
            },
            Err(e) => panic!("Call failed: {:?}", e),
        }
    }


    async fn wait_until_removed(path: &Path, timeout_ms: u64) -> bool {
        timeout(Duration::from_millis(timeout_ms), async {
            loop {
                if !path.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await.is_ok()
    }

    pub fn try_remove_file_until_gone(path: &Path, max_retries: usize) {
        for _ in 0..max_retries {
            if !path.exists() {
                return;
            }

            match fs::remove_file(path) {
                Ok(_) => return,
                Err(e) => {
                    // Possibly locked — wait and retry
                    eprintln!("Retrying delete of {:?} due to error: {}", path, e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }

        panic!("Failed to delete file after {} retries: {:?}", max_retries, path);
    }
}

