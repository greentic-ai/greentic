use anyhow::{bail, Error, Result};
use async_trait::async_trait;
use notify::{Config, Event, EventKind, PollWatcher, Result as NotifyResult, RecursiveMode, Watcher};
use std::{path::{Path, PathBuf}, sync::Arc};
use tokio::{task::JoinHandle, time::{sleep, Duration}};
use tracing::{error, warn};

/// Trait for objects that can be hot-reloaded from disk.
#[async_trait]
pub trait WatchedType: Send + Sync + 'static {
    fn is_relevant(&self, path: &Path) -> bool;
    async fn on_create_or_modify(&self, path: &Path) -> Result<()>;

    async fn on_remove(&self, path: &Path) -> Result<()>;

    async fn reload(&self, path: &Path) -> Result<()> {
        // Default: just call on_create_or_modify
        self.on_create_or_modify(path).await
    }
}


pub struct Watched(pub Box<dyn WatchedType>);

pub async fn watch_dir(
    dir: PathBuf,
    watcher_impl: Arc<dyn WatchedType>,
    exts: &[&str],
    _enable_retry: bool,
) -> Result<JoinHandle<()>,Error> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<NotifyResult<Event>>();

    if !dir.exists() {
        let error = format!("Directory {} to watch does not exist",dir.to_string_lossy());
        warn!(error);
        bail!(error);
    }

    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if watcher_impl.is_relevant(&path) || is_valid_extension(&path, exts) {
            try_reload(&watcher_impl, &path, _enable_retry).await;
        }
    }

    let watcher_impl_clone = watcher_impl.clone();
    let exts_clone: Vec<String> = exts.iter().map(|s| s.to_string()).collect();

    let handle: JoinHandle<()> = tokio::spawn(async move {
        // create & watch inside the task
        let mut watcher = PollWatcher::new(
            move |res| { let _ = tx.send(res); },
            Config::default().with_poll_interval(Duration::from_secs(2)),
        ).expect("failed to create watcher");
        watcher
            .watch(&dir, RecursiveMode::Recursive)
            .expect("failed to watch dir");
        while let Some(res) = rx.recv().await {
            match res {
                Ok(Event { kind: EventKind::Create(_) | EventKind::Modify(_), paths, .. }) => {
                    for path in paths {
                        if watcher_impl_clone.is_relevant(&path) || path.extension().and_then(|e| e.to_str()).map_or(false, |e| exts_clone.contains(&e.to_string())) {
                            let watcher = watcher_impl_clone.clone();
                            let path_clone = path.clone();
                            tokio::spawn(async move {
                                if let Err(e) = watcher.on_create_or_modify(&path_clone).await {
                                    warn!(?path_clone, ?e, "Failed to handle create/modify");
                                }
                            });
                        }
                    }
                }
                Ok(Event { kind: EventKind::Remove(_), paths, .. }) => {
                    for path in paths {
                        if watcher_impl_clone.is_relevant(&path) || path.extension().and_then(|e| e.to_str()).map_or(false, |e| exts_clone.contains(&e.to_string())) {
                            let watcher = watcher_impl_clone.clone();
                            let path_clone = path.clone();
                            tokio::spawn(async move {
                                if let Err(e) = watcher.on_remove(&path_clone).await {
                                    warn!(?path_clone, ?e, "Failed to handle removal");
                                }
                            });
                        }
                    }
                }
                Err(e) => {
                    warn!(?e, "Watcher error");
                }
                _ => {}
            }
        }
    });

    Ok(handle)
}



fn is_valid_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map_or(false, |ext| extensions.iter().any(|&e| e == ext))
}

async fn try_reload(watched: &Arc<dyn WatchedType>, path: &Path, retry: bool) {
    const MAX_RETRIES: usize = 10;

    for attempt in 0..MAX_RETRIES {
        let result = watched.reload(path).await;
        match result {
            Ok(_) => return,
            Err(e) => {
                if !retry || attempt == MAX_RETRIES - 1 {
                    error!("Failed to reload {:?}: {e:?}", path);
                    return;
                } else {
                    warn!("Retrying reload {:?} (attempt {}): {e:?}", path, attempt + 1);
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::collections::HashSet;

    struct DummyWatcher {
        created: Arc<AtomicUsize>,
        removed: Arc<AtomicUsize>,
        allowed_exts: HashSet<String>,
    }

    #[async_trait]
    impl WatchedType for DummyWatcher {
        fn is_relevant(&self, path: &Path) -> bool {
            path.extension()
                .and_then(|e| e.to_str())
                .map(|e| self.allowed_exts.contains(e))
                .unwrap_or(false)
        }

        async fn on_create_or_modify(&self, _path: &Path) -> Result<()> {
            self.created.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn on_remove(&self, _path: &Path) -> Result<()> {
            self.removed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_dummy_watch() {
        let watcher = DummyWatcher {
            created: Arc::new(AtomicUsize::new(0)),
            removed: Arc::new(AtomicUsize::new(0)),
            allowed_exts: ["txt"].iter().map(|s| s.to_string()).collect(),
        };

        let dir = PathBuf::from("./tests/tmp_watcher");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_file.txt");
        let _ = std::fs::write(&path, "hello");

        let handle = tokio::spawn(watch_dir(dir.clone(), Arc::new(watcher), &["txt"], false));

        // Give time for watcher to process
        sleep(Duration::from_millis(300)).await;

        let _ = std::fs::remove_file(path);
        sleep(Duration::from_millis(300)).await;

        // We won't assert here; this is just to ensure no panic occurs
        let _ = handle.await;
    }
}