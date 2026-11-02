//! Thread-count selection helpers.

use std::num::NonZero;
use std::sync::OnceLock;
use std::{io, thread};

use rayon::{ThreadPool, ThreadPoolBuildError, ThreadPoolBuilder};

/// Starts a named worker running `task` until it returns.
pub type ThreadSpawner = fn(name: String, task: Box<dyn FnOnce() + Send>) -> io::Result<()>;

static THREAD_SPAWNER: OnceLock<ThreadSpawner> = OnceLock::new();

/// Installs the host's way to start worker threads.
///
/// wasm32 cannot call `std::thread::spawn`; the embedder (Web Workers sharing the
/// module's memory) supplies this before any pool is built. Native builds ignore
/// it. Returns `false` if a spawner was already installed.
pub fn install_thread_spawner(spawner: ThreadSpawner) -> bool {
    THREAD_SPAWNER.set(spawner).is_ok()
}

/// Builds a rayon pool, starting its workers through the installed
/// [`ThreadSpawner`] on wasm32 and through `std::thread` elsewhere.
///
/// # Errors
/// Returns an error if a worker cannot be started, or on wasm32 if no spawner
/// was installed.
pub fn build_thread_pool(builder: ThreadPoolBuilder) -> Result<ThreadPool, ThreadPoolBuildError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        builder.build()
    }
    #[cfg(target_arch = "wasm32")]
    {
        builder
            .spawn_handler(|worker| {
                let spawner = THREAD_SPAWNER.get().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "no thread spawner installed")
                })?;
                let name = worker.name().unwrap_or("steel-worker").to_owned();
                spawner(name, Box::new(move || worker.run()))
            })
            .build()
    }
}

/// Assumed parallelism when `available_parallelism` is unavailable.
pub const AVAILABLE_PARALLELISM_FALLBACK: usize = 4;

/// Stack size for debug-build threads with deep density-function call chains.
pub const DEBUG_STACK_SIZE: usize = 8 * 1024 * 1024;

/// Returns the host's available parallelism, or [`AVAILABLE_PARALLELISM_FALLBACK`].
#[must_use]
pub fn available_worker_threads() -> usize {
    thread::available_parallelism().map_or(AVAILABLE_PARALLELISM_FALLBACK, NonZero::get)
}

/// Caps an explicit positive worker count to available parallelism, or uses
/// half the available threads with a minimum target of two.
#[must_use]
pub fn worker_threads_for_available(
    configured_threads: Option<usize>,
    available_threads: usize,
) -> usize {
    let available_threads = available_threads.max(1);
    if let Some(configured_threads) = configured_threads.filter(|&threads| threads > 0) {
        return configured_threads.min(available_threads);
    }

    ((available_threads / 2).max(2)).min(available_threads)
}

#[cfg(test)]
mod tests {
    use super::worker_threads_for_available;

    #[test]
    fn explicit_worker_count_is_capped_to_available_threads() {
        assert_eq!(worker_threads_for_available(Some(16), 8), 8);
        assert_eq!(worker_threads_for_available(Some(4), 8), 4);
    }

    #[test]
    fn zero_or_missing_worker_count_uses_auto_default() {
        assert_eq!(worker_threads_for_available(Some(0), 8), 4);
        assert_eq!(worker_threads_for_available(None, 8), 4);
        assert_eq!(worker_threads_for_available(None, 1), 1);
    }
}
