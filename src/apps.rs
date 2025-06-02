// src/app.rs
use std::{fs, io::Write, path::PathBuf, sync::Arc};
use anyhow::{Context, Error};

use tokio::task::JoinHandle;
use tracing::error;

use crate::{
    channel::{manager::{ChannelManager, HostLogger, IncomingHandler}, node::ChannelsRegistry}, config::ConfigManager, executor::Executor, flow::FlowManager, logger::Logger, process::manager::ProcessManager, secret::SecretsManager, state::InMemoryState, watcher::DirectoryWatcher
};

pub struct App
{
    watcher: Option<DirectoryWatcher>,
    tools_task: Option<JoinHandle<()>>,
    flow_task: Option<JoinHandle<()>>,
    channels_task: Option<JoinHandle<()>>,
    flow_manager: Option<Arc<FlowManager>>,
    process_manager: Option<ProcessManager>,
    executor: Option<Arc<Executor>>,
    channel_manager: Option<Arc<ChannelManager>>,
}

impl App {
    pub fn new() -> Self {
        Self{
            watcher: None,
            tools_task:None,
            flow_task:None,
            channels_task:None,
            flow_manager: None,
            process_manager: None,
            executor: None,
            channel_manager: None,
        }
    }
    /// Bootstraps greentic:
    ///   - loads & watches `flows_dir`
    ///   - watches `tools_dir`
    ///   - loads & watches `channels_dir`
    /// Returns (flow_manager, channel_manager).
    pub async fn bootstrap(
        &mut self,
        flows_dir:    PathBuf,
        channels_dir: PathBuf,
        tools_dir:    PathBuf,
        processes_dir: PathBuf,
        config:       ConfigManager,
        logger:       Logger,
        secrets:      SecretsManager,
    ) -> Result<(),Error> {
        // 1) Flow manager & initial load + watcher
        let store = InMemoryState::new();

        // Process Manager
        match ProcessManager::new(processes_dir)
        {
            Ok(mut pm) => {
                match pm.watch_process_dir().await {
                    Ok(watcher) => {
                        self.process_manager = Some(pm.clone());
                        self.watcher = Some(watcher)
                    },
                    Err(error) => {
                        let werror = format!("Could not start up process manager because {:?}",error);
                        error!(werror);
                        return Err(error);
                    },
                }
            },
            Err(err) => {
                let perror = format!("Could not start up process manager because {:?}",err);
                error!(perror);
                return Err(err);
            },
        }
        let process_manager = Arc::new(self.process_manager.to_owned().unwrap());

        // executor
        self.executor = Some(Executor::new(secrets.clone(), logger.clone()));
        let executor = self.executor.clone().unwrap();


        // 2) Executor / Tool‐watcher
        self.tools_task = Some({
            let ex = executor.clone();
            let dir = tools_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = ex.watch_tool_dir(dir).await {
                    error!("Tool‐watcher error: {:?}", e);
                }
            })
        });

        // Channel manager (internally starts its own PluginWatcher over channels_dir)
        let host_logger = HostLogger::new();
        let channel_manager = ChannelManager::new(config, secrets.clone(),host_logger).await?;
        self.channel_manager = Some(channel_manager.clone());


        // flow manager
        let flow_mgr = FlowManager::new(store.clone(), executor.clone(), channel_manager.clone(), process_manager.clone(), secrets.clone());
        self.flow_manager = Some(flow_mgr.clone());

        // Load all existing flows, then watch for changes:
        flow_mgr
            .load_all_flows_from_dir(&flows_dir)
            .await
            .expect("initial load failed");
        let flow_mgr_clone = flow_mgr.clone();
        self.flow_task = Some(tokio::spawn(async move {
            if let Err(e) = flow_mgr_clone.watch_flow_dir(flows_dir).await {
                error!("Flow‐watcher error: {:?}", e);
            }
        }));

        // Register the ChannelsRegistry with the flow and channel manager
        let registry = ChannelsRegistry::new(
            flow_mgr.clone(),channel_manager.clone()).await;
        channel_manager.subscribe_incoming(registry.clone() as Arc<dyn IncomingHandler>);
        
        // then start watching
        self.channel_manager.clone().unwrap().start_all(channels_dir.clone()).await?;


        
        // We don’t need to manually `start()` each channel here; ChannelManager::new()
        // will have already subscribed the watcher and started existing plugins.
        //
        // If you *do* want to eagerly start _all_ channels immediately, you can:
        // for name in channel_mgr.list_channels() {
        //     channel_mgr.start_channel(&name)?;
        // }

        // Tasks are detached; they’ll run for the lifetime of the process.
        // We return the two managers for the caller to drive shutdown or further orchestration.
        Ok(())
    }

    pub async fn shutdown(&self){    
        
        if let Some(handle) = self.flow_task.as_ref() {
            handle.abort();
        };
        if let Some(handle) = self.tools_task.as_ref() {
            handle.abort();
        };
        if let Some(handle) = self.channels_task.as_ref() {
            handle.abort();
        }
        self.channel_manager.clone().unwrap().shutdown_all(true, 2000);
        self.flow_manager.clone().unwrap().shutdown_all().await;
    }
}
/// Called when user runs `greentic init --root <dir>`
pub async fn cmd_init(root: PathBuf) -> Result<(),Error> {
    // 1) create all the directories we need
    let dirs = [
        "greentic/config",
        "greentic/secrets",
        "greentic/logs",
        "greentic/flows/running",
        "greentic/flows/stopped",
        "greentic/plugins/tools",
        "greentic/plugins/channels/running",
        "greentic/plugins/channels/stopped",
        "greentic/plugins/processes",
    ];
    for d in &dirs {
        let path = root.join(d);
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }

    // 2) write config/.env
    let conf_path = root.join("greentic/config/.env");
    if !conf_path.exists() {
        let default_cfg = r#""#;
        fs::write(&conf_path, default_cfg)
            .with_context(|| format!("failed to write {}", conf_path.display()))?;
        println!("Created {}", conf_path.display());
    } else {
        println!("Skipping {}, already exists", conf_path.display());
    }

    // 3) write a sample flow stub in flows/
    let sample = root.join("greentic/flows/running/sample.greentic");
    if !sample.exists() {
        let template = r#"

{
    "id": "sample.greentic",
    "title": "Mock Flow",
    "description": "A sample flow",
        "channels": [
        "mock_inout",
        "mock_middle" 
    ],
    "nodes": {
        "mock_in": {
            "channel": "mock_inout",
            "in": true
        },
        "mock_middle": {
            "channel": "mock_middle",
            "in": true,
            "out": true
        },
        "mock_out": {
            "channel": "mock_inout",
            "out": true
        }
    },
    "connections": {
        "mock_in": [
            "mock_middle"
        ],
        "mock_middle": [
            "mock_out"
        ]
    }
}
        "#;
        let mut f = fs::File::create(&sample)
            .with_context(|| format!("failed to create {}", sample.display()))?;
        f.write_all(template.as_bytes())?;
        println!("Created sample flow {}", sample.display());
    } else {
        println!("Skipping {}, already exists", sample.display());
    }

    println!("Greentic directory initialized at {}", root.display());
    Ok(())
}