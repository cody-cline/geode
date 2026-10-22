//! Spawning a background task from a function that is not async.
//!
//! Most of the library spawns with `tokio::spawn`, from inside a task that
//! already has a runtime. A couple of places cannot: [`PortMapper::start`] is
//! an ordinary function the desktop app may call with no runtime entered, and
//! `tokio::spawn` panics there. Those call sites used `tauri::async_runtime`,
//! whose global runtime exists whether or not a caller is inside one — which
//! made port mapping, and so sharing, depend on Tauri for nothing but a thread
//! pool.
//!
//! So they come here instead. With the `desktop` feature this is Tauri's
//! runtime, exactly as before. Without it, the current runtime when there is
//! one — the daemon's own, which is where its work belongs — and otherwise one
//! created on first use and kept, which is all Tauri was being asked for.
//!
//! [`PortMapper::start`]: crate::share::portmap::PortMapper::start

#[cfg(feature = "desktop")]
pub use tauri::async_runtime::{spawn, JoinHandle};

#[cfg(not(feature = "desktop"))]
pub type JoinHandle<T> = tokio::task::JoinHandle<T>;

/// Run `task` in the background, from anywhere.
#[cfg(not(feature = "desktop"))]
pub fn spawn<F>(task: F) -> JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.spawn(task),
        Err(_) => fallback().spawn(task),
    }
}

/// A runtime for callers who have none, built the first time one asks.
///
/// Never built in the daemon, which spawns from inside its own runtime; this
/// is for a caller reached from a plain function on a plain thread.
#[cfg(not(feature = "desktop"))]
fn fallback() -> &'static tokio::runtime::Handle {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Runtime::new().expect("a background runtime could not be created")
        })
        .handle()
}

#[cfg(test)]
mod tests {
    /// The contract both builds owe their callers, and the one `tokio::spawn`
    /// does not keep: a plain function, no runtime entered, and a task that
    /// has to run anyway.
    #[test]
    fn a_task_spawned_outside_any_runtime_still_runs() {
        let (tx, rx) = std::sync::mpsc::channel();
        super::spawn(async move {
            let _ = tx.send("ran");
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok("ran")
        );
    }
}
