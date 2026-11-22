//! What every service does to its own [`ServiceRuntime`], and the guard that
//! stops its task: one copy for Caddy, php-fpm and the database servers.

use marsa_core::types::{Event, LogStream, ServiceState};
use tokio::sync::broadcast;
use tokio::task::AbortHandle;

use crate::daemon::services::supervisor::{ServiceRuntime, now_ms};

/// Aborts a task when this guard goes: a wrapper task that `stop_procs`
/// aborts takes the task it waits for along.
pub(crate) struct AbortOnDrop(pub(crate) AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl ServiceRuntime {
    /// Adds a line from the daemon to the service's recent lines, so `marsa
    /// logs <service>` shows why it did not start. The caller holds the lock,
    /// like the supervisor, so events come in `seq` order.
    pub(crate) fn note(&mut self, events: &broadcast::Sender<Event>, text: &str) {
        let line = self.logs.push(LogStream::Stderr, text, now_ms());
        // An error only means that nobody listens right now.
        let _ = events.send(Event::Log(line));
    }

    /// Sets `Failed { code }` and tells every reader, under the caller's
    /// lock like the supervisor, so events come in the order of the changes.
    /// Only while no task runs the service: the supervisor owns the status
    /// then.
    pub(crate) fn set_failed(&mut self, events: &broadcast::Sender<Event>, code: &str) {
        let status = &mut self.status;
        status.state = ServiceState::Failed {
            code: code.to_owned(),
        };
        status.pid = None;
        status.since_ms = Some(now_ms());
        // An error only means that nobody listens right now.
        let _ = events.send(Event::ServiceChanged(status.clone()));
    }
}
