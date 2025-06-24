// src/runtime.rs
use once_cell::sync::OnceCell;
use tokio::runtime::{Handle, Runtime};
use std::future::Future;

static TOKIO_RUNTIME: OnceCell<Runtime> = OnceCell::new();

/// Initializes a global multi-threaded Tokio runtime if not already initialized.
/// Safe to call multiple times.
pub fn init_tokio_runtime() -> &'static Runtime {
    TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to build Tokio runtime")
    })
}

/// Spawns an async task on the global runtime.
/// Useful for FFI-style calls or plugin boundaries.
pub fn spawn_async_task<F>(fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let rt = init_tokio_runtime();
    rt.spawn(fut);
}

/// Runs a future to completion using the global runtime.
/// Safe to use inside FFI or sync contexts.
pub fn run_blocking<F, R>(fut: F) -> R
where
    F: Future<Output = R> + Send,
    R: Send + 'static,
{
    let rt = init_tokio_runtime();
    rt.block_on(fut)
}

/// Gets a handle to the current global runtime.
/// Can be used for spawning or entering the runtime manually.
pub fn get_handle() -> Handle {
    init_tokio_runtime().handle().clone()
}
