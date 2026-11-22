//! One run of a database server under the supervisor: its spawn, the wait
//! for its first answer, and how a run that ended by itself is read.

use marsa_core::data_dir::DataDir;
use std::path::PathBuf;
use std::sync::Arc;

use marsa_core::databases::{ServerKind, ServiceBusy};
use marsa_core::services::ServiceSpec;
use marsa_core::types::LogStream;
use tokio::sync::watch;

use super::super::orphan::{Recorded, RunRecord};
use super::super::{mysql, postgres, valkey};
use super::{ActionError, ServerManager};
use crate::daemon::state::Daemon;
use crate::daemon::supervisor::{ServiceHandle, Supervisor, lock};

/// How a run that ended before it took connections says that a lock file
/// is held.
pub(super) enum Refusal {
    /// PostgreSQL: its cluster, for its `postmaster.pid`, and its socket, for
    /// the name of its lock file (`None` for TCP only).
    Postgres {
        data: PathBuf,
        socket: Option<PathBuf>,
    },
    /// Not read: MySQL's own check before the start covers its data
    /// folder, and Valkey takes no lock on its own.
    None,
}

pub(super) enum Ready {
    Yes,
    /// The run ended by itself first: the status says why.
    Ended,
    TimedOut,
}

impl ServerManager {
    /// Spawns the server under the supervisor, and registers the task of
    /// the run in `Daemon::procs`. That task writes `run/<id>.pid` as soon as
    /// the process runs one of `servers` ([`RunRecord`]), and gives what the
    /// record came to through the receiver this returns.
    pub(super) fn spawn(
        &self,
        daemon: &Daemon,
        spec: ServiceSpec,
        servers: Vec<PathBuf>,
    ) -> Result<watch::Receiver<Recorded>, ActionError> {
        let logs_dir = DataDir::new(&daemon.data_dir).logs();
        let program = spec.program.clone();
        let runtime = Arc::clone(&self.runtime);
        let events = daemon.events.subscribe();
        let handle = Supervisor::spawn_on(spec, daemon.events.clone(), &logs_dir, runtime)
            .map_err(|err| {
                self.fail(&daemon.events, err.code(), &err.to_string());
                ActionError::ProgramNotAbsolute(err)
            })?;
        *lock(&self.started) = Some(program);
        let ServiceHandle {
            cancel,
            task,
            runtime,
            stop_limit,
        } = handle;
        let (recorded, recording) = watch::channel(Recorded::Pending);
        let run = RunRecord {
            kind: self.kind,
            probes: Arc::clone(&self.probes),
            data_dir: daemon.data_dir.clone(),
            servers,
            runtime: Arc::clone(&runtime),
            events,
            recorded,
        };
        let handle = ServiceHandle {
            cancel,
            task: tokio::spawn(run.follow(task)),
            runtime,
            stop_limit,
        };
        // A handle this replaces belongs to a run that has ended.
        daemon.add_proc(self.kind.id(), handle);
        Ok(recording)
    }

    /// Asks `127.0.0.1:<port>` ([`Self::answers`]) until our server answers,
    /// its run ends, or the limit. The check before the start found the port
    /// free, so what answers is our server.
    pub(super) async fn wait_ready(&self, daemon: &Daemon, port: u16) -> Ready {
        let kind = self.kind;
        let poll = async {
            loop {
                if !daemon.proc_alive(kind.id()) {
                    return Ready::Ended;
                }
                if self.answers(port).await {
                    return Ready::Yes;
                }
                tokio::time::sleep(super::READY_POLL).await;
            }
        };
        tokio::time::timeout(self.ready_limit, poll)
            .await
            .unwrap_or(Ready::TimedOut)
    }

    /// Whether the server takes connections on `127.0.0.1:<port>`, by its
    /// own protocol ([`postgres::answers`]).
    async fn answers(&self, port: u16) -> bool {
        match self.kind {
            ServerKind::Postgres => postgres::answers(port).await,
            ServerKind::Mysql => mysql::answers(port).await,
            ServerKind::Valkey => valkey::answers(port).await,
        }
    }

    /// Why a run that ended by itself before it took connections did not
    /// start, when the server refused because a live process holds one of
    /// its lock files ([`postgres::lock_refusal`]), from this run's stderr
    /// lines, from `first_seq`.
    pub(super) async fn lock_refusal(
        &self,
        first_seq: u64,
        refusal: &Refusal,
        port: u16,
    ) -> Option<(ServiceBusy, String)> {
        let recent = self.recent_logs();
        let stderr: Vec<&str> = recent
            .iter()
            .filter(|line| line.seq >= first_seq && line.stream == LogStream::Stderr)
            .map(|line| line.text.as_str())
            .collect();
        match refusal {
            Refusal::Postgres { data, socket } => {
                let socket = socket.as_deref();
                postgres::lock_refusal(&*self.probes, &stderr, data, socket, port).await
            }
            Refusal::None => None,
        }
    }
}
