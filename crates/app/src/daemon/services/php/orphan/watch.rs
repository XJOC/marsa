//! The watcher that writes the pid file after each spawn of a pool and deletes it
//! after each exit.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use marsa_core::orphan::Liveness;
use marsa_core::ps::StartTime;
use marsa_core::types::{Event, ServiceId, ServiceState};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::JoinHandle;
use tracing::warn;

use super::{POLL, PidFile, RECORD_LIMIT, Recorded};
use crate::daemon::services::probe::ProcessProbe;
use crate::daemon::services::runtime::AbortOnDrop;
use crate::daemon::services::supervisor::{ServiceHandle, ServiceRuntime, lock};

/// Puts a watcher between the daemon and the supervisor's task of one pool
/// run: it writes the pid file after each spawn of `program` and deletes it
/// after each exit. Its task replaces the supervisor's in the handle, so a
/// stop waits for it. `lines` must be subscribed before the spawn, so the
/// first `running` is not missed.
pub fn watch(
    handle: ServiceHandle,
    lines: broadcast::Receiver<Event>,
    pid_file: PidFile,
    program: PathBuf,
    processes: Arc<dyn ProcessProbe>,
) -> ServiceHandle {
    let ServiceHandle {
        cancel,
        task,
        runtime,
        stop_limit,
    } = handle;
    let watcher = Watcher {
        lines,
        runtime: Arc::clone(&runtime),
        file: Recording {
            pid_file,
            program,
            processes,
            spawn: None,
        },
    };
    let task = tokio::spawn(watcher.run(task));
    ServiceHandle {
        cancel,
        task,
        runtime,
        stop_limit,
    }
}

struct Watcher {
    lines: broadcast::Receiver<Event>,
    runtime: Arc<Mutex<ServiceRuntime>>,
    file: Recording,
}

impl Watcher {
    async fn run(self, mut supervisor: JoinHandle<()>) {
        // `stop_procs` aborts this task after its time limit; the abort must
        // reach the supervisor too, so the `Child` is dropped and killed.
        let _abort = AbortOnDrop(supervisor.abort_handle());
        let Watcher {
            mut lines,
            runtime,
            mut file,
        } = self;
        let id = lock(&runtime).status.id;
        loop {
            let status = tokio::select! {
                result = &mut supervisor => {
                    report(id, result);
                    break;
                }
                event = lines.recv() => match event {
                    Ok(Event::ServiceChanged(status)) if status.id == id => status,
                    Ok(_) => continue,
                    // Too far behind: the status is current.
                    Err(RecvError::Lagged(_)) => lock(&runtime).status.clone(),
                    // The daemon is gone: wait for the process alone.
                    Err(RecvError::Closed) => {
                        report(id, (&mut supervisor).await);
                        break;
                    }
                },
                () = file.due() => {
                    file.look().await;
                    continue;
                }
            };
            match (&status.state, status.pid) {
                // The supervisor says `running` right after each spawn.
                (ServiceState::Running, Some(pid)) => file.spawned(pid).await,
                // php-fpm is about to exit: no file for it any more.
                (ServiceState::Stopping, _) => file.stopping(),
                // php-fpm has exited, so its pid may be anyone's from now on.
                (ServiceState::Failed { .. } | ServiceState::Stopped, _) => file.exited(),
                _ => {}
            }
        }
        file.exited();
    }
}

/// The pid file of one pool run, as its watcher keeps it.
struct Recording {
    pid_file: PidFile,
    program: PathBuf,
    processes: Arc<dyn ProcessProbe>,
    /// The last spawn, until it exits.
    spawn: Option<Spawn>,
}

/// One spawn of php-fpm, and how far its pid file got. A `running` with its
/// pid again (after `RecvError::Lagged`, say) is the same spawn: it starts
/// no new look.
struct Spawn {
    pid: u32,
    /// Its start time, once a look read it: a later look that shows another
    /// one means that the pid changed hands.
    started: Option<StartTime>,
    file: FileState,
}

enum FileState {
    /// Not written yet: the next look is due at `next`. No look is planned
    /// once `until` has passed, so the last one may start up to [`POLL`]
    /// after it.
    Looking {
        next: tokio::time::Instant,
        until: tokio::time::Instant,
    },
    /// The file names it.
    Written,
    /// No file, and no more looks.
    Never,
}

impl Recording {
    /// After a spawn of `pid`: writes the file, or looks again soon.
    async fn spawned(&mut self, pid: u32) {
        if self.spawn.as_ref().is_some_and(|spawn| spawn.pid == pid) {
            return;
        }
        // The run before it has exited, even if its end was missed.
        self.exited();
        let now = tokio::time::Instant::now();
        self.spawn = Some(Spawn {
            pid,
            started: None,
            file: FileState::Looking {
                next: now,
                until: now + RECORD_LIMIT,
            },
        });
        self.look().await;
    }

    /// Ready when the next look is due; never while none is planned.
    async fn due(&self) {
        match &self.spawn {
            Some(Spawn {
                file: FileState::Looking { next, .. },
                ..
            }) => tokio::time::sleep_until(*next).await,
            _ => std::future::pending().await,
        }
    }

    /// One look at the spawn ([`PidFile::record`]). Another one is due after
    /// [`POLL`] while it is [`Recorded::NotYet`], until [`RECORD_LIMIT`]
    /// after the spawn.
    async fn look(&mut self) {
        let Some(spawn) = &mut self.spawn else {
            return;
        };
        let FileState::Looking { until, .. } = spawn.file else {
            return;
        };
        let (pid, then) = (spawn.pid, spawn.started);
        let found = self
            .pid_file
            .record(&*self.processes, pid, &self.program, then);
        spawn.file = match found.await {
            Recorded::Written(record) => {
                spawn.started = Some(record.started);
                FileState::Written
            }
            Recorded::Never => FileState::Never,
            Recorded::NotYet(now) => {
                if let Liveness::Alive { started, .. } = &now {
                    spawn.started.get_or_insert(*started);
                }
                let at = tokio::time::Instant::now();
                if at < until {
                    let next = at + POLL;
                    FileState::Looking { next, until }
                } else {
                    warn!(
                        pid,
                        ?now,
                        "php-fpm pid file not written: the pid is not verified as our php-fpm"
                    );
                    FileState::Never
                }
            }
        };
    }

    /// A stop is under way: php-fpm is about to exit, so no more looks.
    fn stopping(&mut self) {
        if let Some(spawn) = &mut self.spawn
            && matches!(spawn.file, FileState::Looking { .. })
        {
            spawn.file = FileState::Never;
        }
    }

    /// The php-fpm has exited, or a new one replaced it: its pid may be
    /// anyone's from now on. Deletes only a file this watcher wrote.
    fn exited(&mut self) {
        if let Some(Spawn {
            file: FileState::Written,
            ..
        }) = self.spawn.take()
        {
            self.pid_file.remove();
        }
    }
}

fn report(id: ServiceId, result: Result<(), tokio::task::JoinError>) {
    if let Err(err) = result {
        warn!(service = %id, %err, "the php-fpm supervisor task failed");
    }
}
