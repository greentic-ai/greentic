// src/app.rs
use std::{fs::{self, File}, io::{stdin, stdout, Write}, path::{Path, PathBuf}, sync::Arc};
use anyhow::{bail, Context, Error};

use channel_plugin::message::LogLevel;
use reqwest::{header::{HeaderValue, AUTHORIZATION}, Client};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tracing::{error, info};
use anyhow::{anyhow, Result};
use crate::{
    channel::{manager::{ChannelManager,IncomingHandler}, node::ChannelsRegistry}, config::ConfigManager, executor::Executor, flow::{manager::FlowManager, session::InMemorySessionStore,}, logger::{LogConfig, Logger}, process::manager::ProcessManager, secret::{EnvSecretsManager, SecretsManager}, watcher::DirectoryWatcher
};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Makes a file executable on Unix. On Windows, this is a no-op.
pub fn make_executable<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let metadata = fs::metadata(&path)?;
        let mut permissions = metadata.permissions();
        // Add execute bit for owner/group/others
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(&path, permissions)?;
    }

    #[cfg(windows)]
    {
        // No-op: Windows uses extensions like `.exe` to determine executability
    }

    Ok(())
}
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
        session_timeout: u64,
        flows_dir:     PathBuf,
        channels_dir:  PathBuf,
        tools_dir:     PathBuf,
        processes_dir: PathBuf,
        config:        ConfigManager,
        logger:        Logger,
        log_level:     LogLevel,
        log_dir:       Option<String>,
        otel_endpoint: Option<String>,
        secrets:       SecretsManager,
    ) -> Result<(),Error> {
        // 1) Flow manager & initial load + watcher
        let store = InMemorySessionStore::new(session_timeout);
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
        let log_config = LogConfig::new(log_level, log_dir, otel_endpoint);
        let channel_manager = ChannelManager::new(config, secrets.clone(),store.clone(),log_config).await?;
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
        "config",
        "secrets",
        "logs",
        "flows/running",
        "flows/stopped",
        "plugins/tools",
        "plugins/channels/running",
        "plugins/channels/stopped",
        "plugins/processes",
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

    // 3) registration
    println!("Greentic registration so you can download flows, channels, tools,...");
    println!("📄 Please review our Terms & Conditions:");
    println!("   👉 https://greentic.ai/assets/tandcs.html");

    loop {
        print!("Do you accept the Terms & Conditions? [Y,n]: ");
        stdout().flush().unwrap();

        let mut response = String::new();
        stdin().read_line(&mut response).unwrap();
        let response = response.trim().to_lowercase();

        match response.as_str() {
            "" | "y" | "yes" => {
                println!("✅ Thank you for accepting the Terms & Conditions.");
                break;
            }
            "n" | "no" => {
                println!("❌ You must accept the Terms & Conditions to continue.");
                std::process::exit(1); // exit the program
            }
            _ => {
                println!("⚠️  Invalid input. Please type 'y' or 'n'.");
            }
        }
    }
    
    print!("Please provide your email:");
    stdout().flush().unwrap(); // ensure prompt shows before user types

    let mut email = String::new();
    stdin().read_line(&mut email).unwrap();
    let email = email.trim().to_string();

    let mut marketing_consent = false;
    loop {
        print!("Are you ok to be contacted by email about new features and other Greentic services? [Y,n]: ");
        stdout().flush().unwrap();

        let mut response = String::new();
        stdin().read_line(&mut response).unwrap();
        let response = response.trim().to_lowercase();

        match response.as_str() {
            "" | "y" | "yes" => {
                println!("✅ Thank you.");
                marketing_consent = true;
                break;
            }
            "n" | "no" => {
                println!("❌ Thank you, we will not store your email.");
                break;
            }
            _ => {
                println!("⚠️  Invalid input. Please type 'y' or 'n'.");
            }
        }
    }

    let client = Client::new();
    let res = client
        .post("https://greenticstore.com/register")
        .json(&RegisterRequest { email: &email })
        .send()
        .await?;

    let _ = match res.json::<Verifying>().await{
        Ok(verifying) => {
            info!("Verifying status: {}",verifying.status);
            verifying
        },
        Err(err) => {
            error!("Error veryifying: {:?}",err);
            bail!("Could not continue verification because {:?}",err.to_string());
        },
    };
    println!("A verifying code was send to your email. Please reproduce it here.");
    print!("Code: ");
    stdout().flush().unwrap();

    let mut code = String::new();
    stdin().read_line(&mut code).unwrap();
    let code = code.trim(); // remove newline

    let body = VerifyRequest {
        email: &email,
        code,
        marketing_consent,
    };

    let res = client
        .post("https://greenticstore.com/verify")
        .json(&body)
        .send()
        .await?;

    let verified = match res.json::<Verified>().await {
        Ok(response) => {
            info!("Verified status: {}",response.status);
            response
        },
        Err(err) => {
            error!("Error verifying: {:?}", err);
            bail!("Could not continue verification because {:?}", err.to_string());
        }
    };

    let token = &verified.user_token;

    let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(Path::new("greentic/secrets").to_path_buf())));
    let result = secrets_manager.add_secret("GREENTIC_TOKEN", token).await;

    if result.is_err() {
        bail!("Could not add the GREENTIC_TOKEN={} to the secrets. Please add the token manually. Not doing so will not enable you to download from the Greentic store.",token);
    }

    let out_channels_dir = root.join("greentic/plugins/channels/running");
    let out_tools_dir = root.join("greentic/plugins/tools");
    let out_flows_dir = root.join("greentic/flows/running");
    let platform = detect_host_target();
    let channels = format!("channels/{}",platform);
    let _ = download(token, &channels, "channel_telegram", out_channels_dir.clone()).await;
    let _ = download(token, &channels, "channel_ws", out_channels_dir.clone()).await;
    let _ = download(token, "tools", "weather_api.wasm", out_tools_dir.clone()).await;
    let _ = download(token, "flows", "weather_bot_telegram.ygtc", out_flows_dir.clone()).await;
    let _ = download(token, "flows", "weather_bot_ws.ygtc", out_flows_dir.clone()).await;

    println!("Greentic directory initialized at {}", root.display());
    Ok(())
}

pub fn detect_host_target() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "x86_64") {
            "macos_intel"
        } else {
            "macos_arm"
        }
    } else {
        "linux"
    }
}

pub async fn download(
    token: &String,
    download_type: &str,
    download_file: &str,
    out_dir: PathBuf,
) -> Result<()> {
    let url = format!("https://greenticstore.com/{}/{}", download_type, download_file);
    let output_path = out_dir.join(&download_file);

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", token))?)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "❌ Failed to download '{}'. Status: {}",
            download_file,
            response.status()
        ));
    }

    let bytes = response.bytes().await?;

    let mut file = File::create(&output_path)?;
    file.write_all(&bytes)?;
    if download_type == "channels" {
        make_executable(&output_path)?;
    }

    println!("✅ Downloaded to {:?}", output_path);
    Ok(())
}

#[derive(Serialize)]
struct RegisterRequest<'a> {
    email: &'a str,
}

#[derive(Deserialize, Debug)]
struct Verifying {
    status: String,
}


#[derive(Serialize)]
struct VerifyRequest<'a> {
    email: &'a str,
    code: &'a str,
    marketing_consent: bool,
}

#[derive(Deserialize, Debug)]
struct Verified {
    status: String,
    user_token: String,
}