//! Waiting for a stop signal: Ctrl+C everywhere, SIGTERM on macOS and Linux. Task M3-06.
//!
//! Windows has no SIGTERM: `POST /v1/shutdown` is the normal way to stop the
//! daemon there (architecture.md section 6).

use std::future::Future;

/// Starts listening for the stop signals now, and gives a future that ends
/// when one arrives.
///
/// Call it inside the tokio runtime, before `run/daemon.json` is written: a
/// signal that arrives before the handlers exist kills the process at once,
/// with no clean shutdown. A handler that cannot be set up is logged and never
/// fires; the other ways to stop still work.
pub fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    let (first, second) = listen();
    async move {
        tokio::select! {
            () = first => {}
            () = second => {}
        }
    }
}

/// SIGINT (what Ctrl+C sends) and SIGTERM.
#[cfg(unix)]
fn listen() -> (
    impl Future<Output = ()> + Send + 'static,
    impl Future<Output = ()> + Send + 'static,
) {
    use tokio::signal::unix::{SignalKind, signal};

    let wait = |kind: SignalKind, name: &'static str| {
        let listener = signal(kind).inspect_err(|err| {
            tracing::warn!(signal = name, error = %err, "cannot listen for the signal");
        });
        async move {
            // `None` means the listener closed, which is no request to stop.
            if let Ok(mut listener) = listener
                && listener.recv().await.is_some()
            {
                tracing::info!(signal = name, "stop signal received");
                return;
            }
            std::future::pending().await
        }
    };
    (
        wait(SignalKind::interrupt(), "SIGINT"),
        wait(SignalKind::terminate(), "SIGTERM"),
    )
}

/// Ctrl+C only. The second future never ends.
#[cfg(windows)]
fn listen() -> (
    impl Future<Output = ()> + Send + 'static,
    impl Future<Output = ()> + Send + 'static,
) {
    let listener = tokio::signal::windows::ctrl_c().inspect_err(|err| {
        tracing::warn!(signal = "Ctrl+C", error = %err, "cannot listen for the signal");
    });
    let ctrl_c = async move {
        if let Ok(mut listener) = listener
            && listener.recv().await.is_some()
        {
            tracing::info!(signal = "Ctrl+C", "stop signal received");
            return;
        }
        std::future::pending().await
    };
    (ctrl_c, std::future::pending())
}
