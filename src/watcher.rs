use anyhow::{Result, bail};
use async_trait::async_trait;
use notify::{
    Config, Event, EventKind, PollWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind},
};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    sync::mpsc::UnboundedReceiver,
    task::JoinHandle,
    time::{Duration, sleep},
};
use tracing::{error, warn};

/// A global list of every watcher JoinHandle that has been spawned.
/// We keep it behind a `Mutex<Vec<JoinHandle<()>>>` so we can push new handles
/// as they are created, and later abort() them all at once.
static GLOBAL_WATCHER_HANDLES: Lazy<Mutex<Vec<JoinHandle<()>>>> =
    Lazy::new(|| Mutex::new(Vec::new()));

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

/// A small wrapper around all of the spawned watcher‐tasks for a single directory.
/// Calling `shutdown()` will abort every task in `handles`.
#[derive(Clone)]
pub struct DirectoryWatcher;

impl DirectoryWatcher {
    /// Start watching `dir` for any files whose extension matches `exts` _or_ for
    /// any `WatcherType::is_relevant(path)`‐true paths.  Returns a `DirectoryWatcher`
    /// containing every spawned task.  If `_enable_retry` is true, failed reloads that
    /// return Err will be retried a few times before giving up.
    pub async fn new(
        dir: PathBuf,
        watcher_impl: Arc<dyn WatchedType>,
        exts: &[&str],
        initial_scan: bool,
        enable_retry: bool,
    ) -> Result<DirectoryWatcher> {
        // If the directory doesn’t exist, bail out immediately.
        if !dir.exists() {
            let msg = format!("Directory {} does not exist", dir.to_string_lossy());
            warn!(%msg);
            bail!(msg);
        }

        // 1) On startup: scan the existing entries in `dir` and “reload” each one.
        //    In other words, if file matches, call `try_reload(...)`.
        if initial_scan {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                if watcher_impl.is_relevant(&path) || is_valid_extension(&path, exts) {
                    try_reload(&watcher_impl, &path, enable_retry).await;
                }
            }
        }

        // 2) Set up channels + notify‐watcher so we can receive Future<NotifyResult<Event>>s.
        let (tx, mut rx): (_, UnboundedReceiver<notify::Result<Event>>) =
            tokio::sync::mpsc::unbounded_channel();

        // 3) Spawn a background task that runs the `notify` poll‐watcher.
        //    When events arrive, they get sent down `tx`.  We collect that handle below.
        let dir_clone = dir.clone();
        let handle_watcher = tokio::spawn(async move {
            let mut watcher = PollWatcher::new(
                move |res| {
                    // we ignore send errors, since if `rx` has dropped, no one is listening.
                    let _ = tx.send(res);
                },
                Config::default().with_poll_interval(Duration::from_secs(2)),
            )
            .expect("failed to create PollWatcher");

            watcher
                .watch(&dir_clone, RecursiveMode::Recursive)
                .expect("failed to watch directory");

            // Keep the `watcher` alive inside this blocking context.  This task
            // will never exit on its own; it’s just the glue that pushes FS events
            // into the `tx` channel.
            futures::future::pending::<()>().await;
        });

        // 4) In parallel, spawn a second background task that sits on `rx.recv()`,
        //    examines each Event, and then, if it matches, calls either
        //    `on_create_or_modify` or `on_remove`.
        let watcher_clone = watcher_impl.clone();
        let exts_clone: Vec<String> = exts.iter().map(|s| s.to_string()).collect();
        let handle_dispatch = tokio::spawn(async move {
            while let Some(res) = rx.recv().await {
                match res {
                    Ok(Event {
                        kind:
                            EventKind::Create(CreateKind::Any) | EventKind::Modify(ModifyKind::Data(_)),
                        paths,
                        ..
                    }) => {
                        for path in paths {
                            if watcher_clone.is_relevant(&path)
                                || path
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .map_or(false, |e| exts_clone.contains(&e.to_string()))
                            {
                                let path_clone = path.clone();
                                let watcher_inner = watcher_clone.clone();
                                // Spawn a short‐lived task to handle this single path.
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        watcher_inner.on_create_or_modify(&path_clone).await
                                    {
                                        warn!(?path_clone, ?e, "Failed to handle create/modify");
                                    }
                                });
                            }
                        }
                    }
                    Ok(Event {
                        kind: EventKind::Remove(_),
                        paths,
                        ..
                    }) => {
                        for path in paths {
                            if watcher_clone.is_relevant(&path)
                                || path
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .map_or(false, |e| exts_clone.contains(&e.to_string()))
                            {
                                let path_clone = path.clone();
                                let watcher_inner = watcher_clone.clone();
                                //tokio::spawn(async move {
                                if let Err(e) = watcher_inner.on_remove(&path_clone).await {
                                    warn!(?path_clone, ?e, "Failed to handle removal");
                                }
                                // });
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
        let mut guard = GLOBAL_WATCHER_HANDLES.lock().unwrap();
        guard.append(&mut vec![handle_dispatch, handle_watcher]);
        // 5) Return a DirectoryWatcher holding both handles.
        Ok(DirectoryWatcher {})
    }

    /// Abort all of the spawned watcher‐tasks.  After this returns, no more events
    /// will be dispatched.
    pub fn shutdown(self) {
        let mut guard = GLOBAL_WATCHER_HANDLES.lock().unwrap();
        for handle in guard.drain(..) {
            handle.abort();
        }
    }
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
                    warn!(
                        "Retrying reload {:?} (attempt {}): {e:?}",
                        path,
                        attempt + 1
                    );
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

        //let handle = tokio::spawn(watch_dir(dir.clone(), Arc::new(watcher), &["txt"], false));
        let watcher_arc: Arc<dyn WatchedType> = Arc::new(watcher);
        let dir_watcher = DirectoryWatcher::new(dir.clone(), watcher_arc.clone(), &["txt"], true, false)
            .await
            .unwrap();

        // Give time for watcher to process
        sleep(Duration::from_millis(300)).await;

        let _ = std::fs::remove_file(path);
        sleep(Duration::from_millis(300)).await;

        // We won't assert here; this is just to ensure no panic occurs
        //let _ = handle.await;
        dir_watcher.shutdown();
    }
}
