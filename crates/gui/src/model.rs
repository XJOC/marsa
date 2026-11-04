//! The window's state, and how each message from the bridge changes it.
//!
//! Pure: no drawing and no I/O, so the tests run without a window. The
//! drawing code in `app` only reads a [`Model`].

use std::collections::VecDeque;

use marsa_core::types::{
    Event, Health, LogLine, LogStream, ServiceAction, ServiceState, ServiceStatus,
};

/// The service whose log the window shows.
pub const LOG_SERVICE: &str = "caddy";
/// The most log lines kept. The oldest go first.
pub const MAX_LOG_LINES: usize = 5000;

/// What the bridge sends to the window.
#[derive(Debug)]
pub enum Update {
    /// Connected. A new session: the daemon may have restarted, so the old
    /// table and log are dropped.
    Connected(Health),
    /// The connection ended; the bridge tries again every second.
    Reconnecting,
    /// The daemon cannot be reached or started. A client error code; the
    /// bridge tries again every second.
    Failed(String),
    /// The whole services table.
    Services(Vec<ServiceStatus>),
    /// Recent lines of [`LOG_SERVICE`], oldest first.
    Log(Vec<LogLine>),
    /// A call failed but the connection is fine. A client error code.
    Notice(String),
    Event(Event),
}

/// The status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Connection {
    #[default]
    Connecting,
    Reconnecting,
    Running(Health),
    ShuttingDown,
    Failed(String),
}

/// One log line, without the fields the window does not show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub stream: LogStream,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct Model {
    pub connection: Connection,
    /// Sorted by id.
    pub services: Vec<ServiceStatus>,
    pub log: VecDeque<LogRow>,
    /// The `seq` of the newest line in `log`.
    last_seq: Option<u64>,
    /// The last error that did not break the connection, as a code.
    pub notice: Option<String>,
}

impl Model {
    pub fn update(&mut self, update: Update) {
        match update {
            Update::Connected(health) => {
                *self = Model {
                    connection: Connection::Running(health),
                    ..Model::default()
                };
            }
            Update::Reconnecting => self.disconnect(Connection::Reconnecting),
            Update::Failed(code) => self.disconnect(Connection::Failed(code)),
            Update::Services(mut services) => {
                services.sort_by(|a, b| a.id.cmp(&b.id));
                self.services = services;
            }
            Update::Log(lines) => lines.into_iter().for_each(|line| self.push_log(line)),
            Update::Notice(code) => self.notice = Some(code),
            Update::Event(event) => apply(self, event),
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.connection, Connection::Running(_))
    }

    /// The table shows live state only, so it goes. The log stays: it may
    /// tell why the daemon stopped.
    fn disconnect(&mut self, connection: Connection) {
        self.connection = connection;
        self.services.clear();
    }

    fn push_log(&mut self, line: LogLine) {
        if line.service != LOG_SERVICE || self.last_seq.is_some_and(|last| line.seq <= last) {
            return;
        }
        self.last_seq = Some(line.seq);
        if self.log.len() == MAX_LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(LogRow {
            stream: line.stream,
            text: line.text,
        });
    }
}

/// Applies one daemon event.
pub fn apply(model: &mut Model, event: Event) {
    match event {
        Event::ServiceChanged(status) => {
            // A new run of the log service may get a new log buffer whose
            // `seq` starts again at 0, so the old `seq` must not hide it.
            if status.id == LOG_SERVICE && status.state == ServiceState::Starting {
                model.last_seq = None;
            }
            match model.services.binary_search_by(|s| s.id.cmp(&status.id)) {
                Ok(index) => model.services[index] = status,
                Err(index) => model.services.insert(index, status),
            }
        }
        Event::Log(line) => model.push_log(line),
        // The window has no install view yet (task M3-12 leaves it out).
        Event::Install(_) => {}
        // No sites view yet (task M4-15).
        Event::SitesChanged(_) => {}
        Event::ShuttingDown => model.disconnect(Connection::ShuttingDown),
    }
}

/// Whether the button for `action` makes sense in `state`.
pub fn can(state: &ServiceState, action: ServiceAction) -> bool {
    match action {
        ServiceAction::Start => {
            matches!(state, ServiceState::Stopped | ServiceState::Failed { .. })
        }
        ServiceAction::Stop | ServiceAction::Restart => {
            matches!(state, ServiceState::Starting | ServiceState::Running)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health() -> Health {
        Health {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 4242,
        }
    }

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(health()));
        model
    }

    fn status(id: &str, state: ServiceState) -> ServiceStatus {
        ServiceStatus {
            id: id.into(),
            pid: (state == ServiceState::Running).then_some(7),
            state,
            restarts: 0,
            since_ms: None,
        }
    }

    fn changed(id: &str, state: ServiceState) -> Event {
        Event::ServiceChanged(status(id, state))
    }

    fn log_line(service: &str, seq: u64) -> LogLine {
        LogLine {
            service: service.into(),
            seq,
            at_ms: 0,
            stream: LogStream::Stderr,
            text: format!("line {seq}"),
        }
    }

    fn line(service: &str, seq: u64) -> Event {
        Event::Log(log_line(service, seq))
    }

    fn texts(model: &Model) -> Vec<&str> {
        model.log.iter().map(|row| row.text.as_str()).collect()
    }

    #[test]
    fn a_service_event_changes_the_table() {
        let mut model = connected();
        apply(&mut model, changed("php", ServiceState::Stopped));
        apply(&mut model, changed("caddy", ServiceState::Starting));
        apply(&mut model, changed("caddy", ServiceState::Running));
        let expected = [
            status("caddy", ServiceState::Running),
            status("php", ServiceState::Stopped),
        ];
        assert_eq!(model.services, expected);
    }

    #[test]
    fn the_log_keeps_the_newest_5000_lines() {
        let mut model = connected();
        for seq in 0..6000 {
            apply(&mut model, line("caddy", seq));
        }
        let texts = texts(&model);
        assert_eq!(texts.len(), MAX_LOG_LINES);
        assert_eq!((texts[0], texts[4999]), ("line 1000", "line 5999"));
    }

    #[test]
    fn a_repeated_or_old_seq_is_ignored() {
        let mut model = connected();
        model.update(Update::Log(vec![log_line("caddy", 4)]));
        for seq in [4, 3, 5, 5] {
            apply(&mut model, line("caddy", seq));
        }
        assert_eq!(texts(&model), ["line 4", "line 5"]);
    }

    #[test]
    fn a_new_run_may_start_its_seq_again() {
        let mut model = connected();
        apply(&mut model, line("caddy", 9));
        apply(&mut model, changed("caddy", ServiceState::Starting));
        apply(&mut model, line("caddy", 0));
        assert_eq!(texts(&model), ["line 9", "line 0"]);
    }

    #[test]
    fn lines_of_other_services_are_not_shown() {
        let mut model = connected();
        apply(&mut model, line("php", 0));
        assert!(model.log.is_empty());
    }

    #[test]
    fn shutting_down_changes_the_status_line() {
        let mut model = connected();
        apply(&mut model, changed("caddy", ServiceState::Running));
        apply(&mut model, Event::ShuttingDown);
        assert_eq!(model.connection, Connection::ShuttingDown);
        assert!(model.services.is_empty());
        assert_eq!(
            crate::strings::status_line(&model.connection),
            "Daemon: shutting down"
        );
    }

    #[test]
    fn a_new_session_starts_clean_and_a_lost_one_keeps_the_log() {
        let mut model = connected();
        apply(&mut model, line("caddy", 3));
        model.update(Update::Notice("service_not_found".into()));
        model.update(Update::Reconnecting);
        assert_eq!(model.connection, Connection::Reconnecting);
        assert_eq!(texts(&model), ["line 3"]);

        model.update(Update::Connected(health()));
        assert!(model.is_connected());
        assert!(model.log.is_empty() && model.notice.is_none());
        // The restarted daemon counts from 0 again.
        apply(&mut model, line("caddy", 0));
        assert_eq!(texts(&model), ["line 0"]);
    }

    #[test]
    fn buttons_match_the_state() {
        use ServiceAction::{Restart, Start, Stop};
        let failed = ServiceState::Failed {
            code: "exited".into(),
        };
        let cases = [
            (ServiceState::Stopped, [true, false, false]),
            (failed, [true, false, false]),
            (ServiceState::Starting, [false, true, true]),
            (ServiceState::Running, [false, true, true]),
            (ServiceState::Stopping, [false, false, false]),
        ];
        for (state, expected) in cases {
            assert_eq!(
                [Start, Stop, Restart].map(|a| can(&state, a)),
                expected,
                "{state:?}"
            );
        }
    }
}
