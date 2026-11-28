//! The generic managed process: what to run, when to restart it, and its
//! recent output.
//!
//! Pure data and functions, so this module needs no `full` feature. The
//! daemon's supervisor (`marsa::daemon::services::supervisor`) runs the process.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use crate::types::{LogLine, LogStream, ServiceId};

/// Lines kept in memory per service. The full output is in `logs/<id>.log`.
pub const LOG_LINES: usize = 2000;

/// How to run one managed process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Also the log file name: `logs/<id>.log`, so the daemon picks it,
    /// never the user.
    pub id: ServiceId,
    /// Absolute: `marsad` never searches `PATH` (architecture.md section 10).
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Added to the daemon's own environment.
    pub env: BTreeMap<String, String>,
    /// Taken out of the daemon's own environment: names the program would
    /// read as settings, such as Mailpit's `MP_*`.
    pub env_remove: Vec<String>,
    /// The working folder. `None` keeps the daemon's.
    pub cwd: Option<PathBuf>,
    pub restart: RestartPolicy,
    pub stop: StopMethod,
    /// On macOS and Linux the process leads a process group of its own, and
    /// each signal of the stop, and the last kill, go to the whole group: a
    /// worker's `schedule:run`, and the commands that one starts, end with
    /// it. A server keeps the daemon's group: it stops its own children.
    /// Nothing changes on Windows, where the daemon's Job Object ends them.
    pub own_group: bool,
}

/// A Unix signal that the daemon sends with `/bin/kill`
/// (`platform::process::send_signal`). Windows has none.
///
/// Defined here, not in `platform` (which needs the `full` feature), because
/// a [`StopMethod`] names its signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Asks the process to clean up and stop. PostgreSQL reads it as a
    /// "smart" shutdown that waits for every client, so it never gets it
    /// (ADR 0023).
    Term,
    /// PostgreSQL's "fast" shutdown: it ends the sessions and stops.
    Int,
    /// PostgreSQL's "immediate" shutdown: it stops at once and recovers its
    /// data at the next start.
    Quit,
    /// Cannot be caught: the process ends at once.
    Kill,
}

impl Signal {
    /// The name for logs, for example `SIGTERM`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Int => "SIGINT",
            Self::Quit => "SIGQUIT",
            Self::Kill => "SIGKILL",
        }
    }
}

/// How the supervisor stops a process: each step in turn asks it to stop and
/// waits up to its own limit for the exit. After the last step, a kill.
///
/// A list of steps, not one variant per server, because every stop in
/// ADR 0023 has this shape and only the way to ask differs:
///
/// - Caddy and php-fpm: SIGTERM, then a kill ([`StopMethod::term`]).
/// - PostgreSQL on macOS and Linux: SIGINT, then SIGQUIT, then a kill
///   ([`crate::databases::postgres_stop`]). Never SIGTERM.
/// - Valkey: `SHUTDOWN SAVE` over its own protocol, then SIGTERM, then a
///   kill ([`crate::databases::valkey::valkey_stop`]).
/// - MySQL and PostgreSQL on Windows, which has no signals: the server's own
///   stop program (`mysqladmin shutdown`, `pg_ctl stop -m fast`), then a
///   kill ([`StopAction::Run`]).
///
/// So each new way to ask is one more [`StopAction`] variant, and the steps,
/// their limits, and the final kill stay as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopMethod {
    pub steps: Vec<StopStep>,
}

/// One step of a [`StopMethod`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopStep {
    pub action: StopAction,
    /// How long to wait for the exit before the next step.
    pub wait: Duration,
}

/// How one step asks the process to stop. When the asking fails, the step
/// is skipped, with no wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopAction {
    /// Windows sends no signals: there the step is skipped, with no wait.
    Signal(Signal),
    /// A request to the server's own port, for a server that saves its data
    /// only when its protocol asks it to stop (Valkey's `SHUTDOWN SAVE`).
    Send(TcpRequest),
    /// A program of the server's own that asks it to stop: how MySQL and
    /// PostgreSQL stop cleanly on Windows, which has no signals (ADR 0023,
    /// section 6).
    Run(StopCommand),
}

impl StopAction {
    /// How long the asking itself may take, beyond the step's wait: nothing
    /// for a signal (the supervisor's margin covers `/bin/kill`), the connect
    /// and the write for a [`TcpRequest`], and [`STOP_COMMAND_GRACE`] for a
    /// [`StopCommand`], whose wait starts with the program.
    pub fn limit(&self) -> Duration {
        match self {
            Self::Signal(_) => Duration::ZERO,
            Self::Send(request) => request.limit.saturating_mul(2),
            Self::Run(_) => STOP_COMMAND_GRACE,
        }
    }

    /// The step's name in logs, such as `SIGTERM` or `mysqladmin shutdown`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Signal(signal) => signal.name(),
            Self::Send(request) => request.name,
            Self::Run(command) => command.name,
        }
    }
}

/// How long the supervisor waits, once the server has exited, for a
/// [`StopCommand`] that still runs to end, so its last lines reach the log.
/// `pg_ctl stop -w` looks for the server's exit ten times a second.
pub const STOP_COMMAND_GRACE: Duration = Duration::from_secs(2);

/// A program that asks a server to stop, with fixed arguments made by code,
/// never from input. Absolute: `marsad` never searches `PATH`. Run directly,
/// never through a shell.
///
/// The supervisor runs it with no window and no input, keeps what it prints
/// as lines of the service's log, and waits up to the step's wait, counted
/// from the program's start, for the server's exit. The program's own end
/// does not end that wait: one that failed may have asked all the same, and
/// one that cannot ask (`mysqladmin` when `root` has a password) only costs
/// the wait. A program that cannot start skips the step, as a failed signal
/// does. One that still runs when the step ends is ended with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Its name in logs, such as `mysqladmin shutdown`.
    pub name: &'static str,
    /// What the log says after the program failed: the likely cause, and
    /// what follows for the server.
    pub on_failure: &'static str,
}

/// Bytes the supervisor writes to `127.0.0.1:<port>` over TCP to ask a
/// server to stop. The host is not a field: the daemon talks to loopback
/// only. No shell and no client program: the supervisor writes the bytes
/// itself.
///
/// A server that takes the request closes the connection and exits, with no
/// reply. Any reply is a refusal (Valkey: `-ERR Errors trying to SHUTDOWN`),
/// and the next step comes at once, as it does when nothing listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpRequest {
    /// The server's port on `127.0.0.1`.
    pub port: u16,
    /// Written as they are. Made by code from fixed words, never from
    /// input.
    pub bytes: Vec<u8>,
    /// The request's name in logs, such as `SHUTDOWN SAVE`.
    pub name: &'static str,
    /// For the connect, and again for the write.
    pub limit: Duration,
}

impl StopMethod {
    /// SIGTERM, up to `grace` to exit, then a kill: Caddy and php-fpm.
    pub fn term(grace: Duration) -> Self {
        Self::signals([(Signal::Term, grace)])
    }

    /// Each signal in turn, each with its own limit, then a kill.
    pub fn signals(steps: impl IntoIterator<Item = (Signal, Duration)>) -> Self {
        let step = |(signal, wait)| StopStep {
            action: StopAction::Signal(signal),
            wait,
        };
        Self {
            steps: steps.into_iter().map(step).collect(),
        }
    }

    /// The longest the steps take before the kill: all their waits and the
    /// limits of their requests ([`StopAction::limit`]) added up.
    pub fn total_wait(&self) -> Duration {
        let add = |total: Duration, step: &StopStep| {
            let step_time = step.wait.saturating_add(step.action.limit());
            total.saturating_add(step_time)
        };
        self.steps.iter().fold(Duration::ZERO, add)
    }
}

/// A relative `program`: `marsad` never searches `PATH`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("program {} is not an absolute path", .0.display())]
pub struct ProgramNotAbsolute(pub PathBuf);

impl ProgramNotAbsolute {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        crate::codes::PROGRAM_NOT_ABSOLUTE
    }
}

/// What the supervisor does when the process exits without a stop request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Stay `Failed`.
    Never,
    /// Restart up to `max_retries` times. The wait starts at `initial` and
    /// doubles after each restart, up to `max`.
    OnFailure {
        max_retries: u32,
        initial: Duration,
        max: Duration,
    },
    /// As `OnFailure`, except that a clean exit (code 0) after a run of
    /// some seconds (the supervisor's `CLEAN_RUN_MIN`) waits `initial` and
    /// is not counted: a worker that `php artisan queue:restart` or
    /// `--max-time` ended is not failing. Each restart leaves a note in the
    /// service's log.
    Always {
        max_retries: u32,
        initial: Duration,
        max: Duration,
    },
}

/// How long to wait before the next restart, or `None` to stay `Failed`.
///
/// `attempt` counts the restarts already made since the last start asked by
/// the user (`ServiceStatus::restarts`): 0 before the first restart.
pub fn restart_delay(policy: RestartPolicy, attempt: u32) -> Option<Duration> {
    match policy {
        RestartPolicy::Never => None,
        RestartPolicy::OnFailure { max_retries, .. }
        | RestartPolicy::Always { max_retries, .. }
            if attempt >= max_retries =>
        {
            None
        }
        // `checked_pow` is `None` from 2^32 on: the delay is `max` long before.
        RestartPolicy::OnFailure { initial, max, .. }
        | RestartPolicy::Always { initial, max, .. } => Some(
            2_u32
                .checked_pow(attempt)
                .map_or(max, |factor| initial.saturating_mul(factor))
                .min(max),
        ),
    }
}

/// The last [`LOG_LINES`] lines of one service, oldest first.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    service: ServiceId,
    lines: VecDeque<LogLine>,
    /// Never reset, not even by a restart, so a client can find gaps.
    next_seq: u64,
}

impl LogBuffer {
    pub fn new(service: ServiceId) -> Self {
        Self {
            service,
            lines: VecDeque::new(),
            next_seq: 0,
        }
    }

    /// Stores one line without its line ending (`\n` or `\r\n`), drops the
    /// oldest line when full, and returns the stored line.
    pub fn push(&mut self, stream: LogStream, text: &str, at_ms: u64) -> LogLine {
        let line = LogLine {
            service: self.service,
            seq: self.next_seq,
            at_ms,
            stream,
            text: text.trim_end_matches(['\r', '\n']).to_owned(),
        };
        self.next_seq += 1;
        if self.lines.len() == LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line.clone());
        line
    }

    /// A copy of the kept lines, oldest first.
    pub fn recent(&self) -> Vec<LogLine> {
        self.lines.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn on_failure(max_retries: u32) -> RestartPolicy {
        RestartPolicy::OnFailure {
            max_retries,
            initial: 100 * MS,
            max: 1000 * MS,
        }
    }

    #[test]
    fn restart_delay_doubles_up_to_max_then_stops() {
        let delays: Vec<_> = (0..6).map(|n| restart_delay(on_failure(5), n)).collect();
        let ms = |n: u32| Some(n * MS);
        assert_eq!(delays, [ms(100), ms(200), ms(400), ms(800), ms(1000), None]);
        // No overflow, however many restarts.
        assert_eq!(restart_delay(on_failure(u32::MAX), 40), Some(1000 * MS));
    }

    #[test]
    fn stop_methods_keep_each_signal_and_its_limit_in_order() {
        let step = |signal, ms: u32| StopStep {
            action: StopAction::Signal(signal),
            wait: ms * MS,
        };
        // Caddy and php-fpm: one SIGTERM step, as before this type.
        assert_eq!(
            StopMethod::term(2000 * MS).steps,
            [step(Signal::Term, 2000)]
        );
        // PostgreSQL's ladder (ADR 0023).
        let ladder = StopMethod::signals([(Signal::Int, 10_000 * MS), (Signal::Quit, 2000 * MS)]);
        let want = [step(Signal::Int, 10_000), step(Signal::Quit, 2000)];
        assert_eq!(ladder.steps, want);
        assert_eq!(Signal::Quit.name(), "SIGQUIT");
        assert_eq!(ladder.total_wait(), 12_000 * MS);
        assert_eq!(StopMethod::term(2000 * MS).total_wait(), 2000 * MS);
        assert_eq!(StopMethod::signals([]).total_wait(), Duration::ZERO);
        let endless = StopMethod::signals([(Signal::Int, Duration::MAX), (Signal::Quit, MS)]);
        assert_eq!(endless.total_wait(), Duration::MAX);
    }

    /// A request may take its connect and its write before its wait, so
    /// the shutdown gives the whole stop that much more.
    #[test]
    fn a_request_counts_its_connect_and_its_write() {
        let request = TcpRequest {
            port: 6379,
            bytes: b"x".to_vec(),
            name: "X",
            limit: 2000 * MS,
        };
        let send = StopAction::Send(request);
        assert_eq!(send.limit(), 4000 * MS);
        assert_eq!(StopAction::Signal(Signal::Term).limit(), Duration::ZERO);
        let mut stop = StopMethod::term(5000 * MS);
        stop.steps.insert(
            0,
            StopStep {
                action: send,
                wait: 10_000 * MS,
            },
        );
        assert_eq!(stop.total_wait(), 19_000 * MS);
    }

    /// A stop program's wait starts with it; after the server's exit it may
    /// still need a moment for its last lines.
    #[test]
    fn a_stop_program_adds_only_its_grace_to_its_wait() {
        let run = StopAction::Run(StopCommand {
            program: PathBuf::from("/m/runtimes/x/bin/stop"),
            args: vec!["now".to_owned()],
            name: "stop now",
            on_failure: "",
        });
        assert_eq!(run.limit(), STOP_COMMAND_GRACE);
        assert_eq!(run.name(), "stop now");
        assert_eq!(StopAction::Signal(Signal::Int).name(), "SIGINT");
        let stop = StopMethod {
            steps: vec![StopStep {
                action: run,
                wait: 30_000 * MS,
            }],
        };
        assert_eq!(stop.total_wait(), 30_000 * MS + STOP_COMMAND_GRACE);
    }

    #[test]
    fn restart_delay_never_restarts_with_never_or_zero_retries() {
        assert_eq!(restart_delay(RestartPolicy::Never, 0), None);
        assert_eq!(restart_delay(on_failure(0), 0), None);
    }

    #[test]
    fn log_buffer_keeps_the_last_lines_with_growing_seq() {
        let mut logs = LogBuffer::new(ServiceId::Caddy);
        for n in 0..=LOG_LINES {
            logs.push(LogStream::Stdout, &format!("line {n}"), 7);
        }
        let recent = logs.recent();
        assert_eq!(recent.len(), LOG_LINES);
        assert_eq!((recent[0].seq, recent[0].text.as_str()), (1, "line 1"));
        assert!(recent.windows(2).all(|w| w[1].seq == w[0].seq + 1));
        assert_eq!(recent[LOG_LINES - 1].service, ServiceId::Caddy);
    }

    #[test]
    fn log_buffer_strips_the_line_ending() {
        let mut logs = LogBuffer::new(ServiceId::Caddy);
        let line = logs.push(LogStream::Stderr, "ready\r\n", 5);
        assert_eq!(
            (line.text.as_str(), line.stream, line.at_ms),
            ("ready", LogStream::Stderr, 5)
        );
        assert_eq!(logs.push(LogStream::Stdout, "a\r", 5).text, "a");
    }
}
