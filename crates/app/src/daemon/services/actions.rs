//! The turn every service action waits for: one action at a time per
//! service, and a wait that ends when the daemon stops.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

/// The wait for the turn ran out, or the daemon began to stop. Each service
/// turns it into its own `ActionError::Busy` (`service_busy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Busy;

/// Takes the turn of a service's actions. Another action that runs is waited
/// for up to `wait`; waits end in the order they began. `on_wait` runs once,
/// only when the turn is not free at once (for the log line).
///
/// The returned guard proves that no other action runs; it is let go when it
/// drops, also when the caller is dropped half way. `busy` is a
/// `tokio::sync::Mutex` held across `.await` as a serialization token, never
/// as a data lock (manager decision 2026-11-14).
///
/// # Errors
///
/// [`Busy`] when the wait runs out or `shutdown` is cancelled while waiting.
pub(crate) async fn take_turn(
    busy: &Arc<Mutex<()>>,
    shutdown: &CancellationToken,
    wait: Duration,
    on_wait: impl FnOnce(),
) -> Result<OwnedMutexGuard<()>, Busy> {
    match Arc::clone(busy).try_lock_owned() {
        Ok(turn) => Ok(turn),
        Err(_) => {
            on_wait();
            let turn = tokio::time::timeout(wait, Arc::clone(busy).lock_owned());
            tokio::select! {
                () = shutdown.cancelled() => Err(Busy),
                turn = turn => turn.map_err(|_| Busy),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn() -> Arc<Mutex<()>> {
        Arc::default()
    }

    #[tokio::test]
    async fn a_free_turn_is_taken_at_once() {
        let mut waited = false;
        let guard = take_turn(&turn(), &CancellationToken::new(), Duration::ZERO, || {
            waited = true;
        })
        .await;
        assert!(guard.is_ok());
        assert!(!waited);
    }

    #[tokio::test]
    async fn a_turn_that_does_not_come_in_time_is_busy() {
        let busy = turn();
        let _held = Arc::clone(&busy).lock_owned().await;
        let mut waited = false;
        let result = take_turn(
            &busy,
            &CancellationToken::new(),
            Duration::from_millis(20),
            || waited = true,
        )
        .await;
        assert_eq!(result.err(), Some(Busy));
        assert!(waited);
    }

    #[tokio::test]
    async fn a_turn_that_comes_in_time_is_taken() {
        let busy = turn();
        let held = Arc::clone(&busy).lock_owned().await;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(held);
        });
        let result = take_turn(
            &busy,
            &CancellationToken::new(),
            Duration::from_secs(5),
            || {},
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn the_daemon_stopping_ends_the_wait() {
        let busy = turn();
        let _held = Arc::clone(&busy).lock_owned().await;
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let result = take_turn(&busy, &shutdown, Duration::from_secs(60), || {}).await;
        assert_eq!(result.err(), Some(Busy));
    }
}
