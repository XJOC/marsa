//! A worker that a crashed `marsad` left running (the rules of ADR 0009, as
//! for the database servers, ADR 0023 section 8). macOS and Linux only
//! ([`process::WORKER_ORPHAN_RECOVERY`]): on Windows the Job Object ends
//! the workers with the daemon (ADR 0017).
//!
//! - After each spawn, `run/<id>.pid` (`run/queue-shop.pid`) is written
//!   once the new process runs the worker's PHP and is a direct child of this
//!   daemon with that start time, and deleted after each exit
//!   (`php::orphan::watch`, as for the php-fpm pools: a worker restarts, so
//!   each run gets its own record).
//! - Before each start, [`clear`] reads that record: a process that runs the
//!   PHP CLI of an installed version of this data folder and started at the
//!   recorded time is ours, and gets the signals of the worker's stop method
//!   (SIGTERM, which lets `queue:work` end its job), then SIGKILL, each only
//!   after the same check passes again ([`pid_record::clear`]).
//! - A record of a process that is gone, or runs another program now, is
//!   deleted with no signal. One that cannot be told gets no signal: the
//!   worker starts, with a line that says so. A worker has no port that
//!   would refuse a second copy, as a server's does, so the check looks
//!   [`pid_record::STOP_LOOKS`] times before it gives up. One of ours that
//!   is still there after SIGKILL keeps the worker from starting
//!   (`worker_orphan_stuck`): two would take the same jobs.
//!
//! The parent of the old process is not checked: the program it runs and
//! its start time say it is ours, as for the database servers, and only one
//! daemon runs at a time (`run/daemon.lock`). The signals reach the PHP
//! process only: a `schedule:run` it started is not stopped.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use marsa_core::codes;
use marsa_core::data_dir::DataDir;
use marsa_core::platform::process::{self, Signal};
use marsa_core::recipes::Platform;
use marsa_core::services::StopMethod;
use marsa_core::types::{Event, ServiceId};
use tokio::sync::broadcast;

use crate::daemon::services::php::orphan::{PidFile, installed_programs, watch};
use crate::daemon::services::pid_record::{self, Cleared, Owner, Stuck};
use crate::daemon::services::probe::ProcessProbe;
use crate::daemon::services::supervisor::{ServiceHandle, lock};

/// `run/<id>.pid` of `data_dir`: `run/queue-shop.pid`. Always one file in
/// `run/`: a worker's id holds a valid site name only.
pub(super) fn pid_path(data_dir: &Path, id: ServiceId) -> PathBuf {
    DataDir::new(data_dir).run().join(format!("{id}.pid"))
}

/// What happened to an orphan of worker `id` before its start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Before {
    /// Nothing of ours was found, or only a stale record: start.
    Clear,
    /// The line for the worker's log, then start.
    Note(String),
    /// Our old worker still runs: the line, and do not start.
    Stuck(String),
}

/// Stops worker `id`'s orphan, if there is one of ours (see the module).
pub(super) async fn clear(
    probes: &dyn ProcessProbe,
    data_dir: &Path,
    platform: Option<Platform>,
    id: ServiceId,
    stop: &StopMethod,
) -> Before {
    let Some(platform) = platform else {
        return Before::Clear;
    };
    if !process::WORKER_ORPHAN_RECOVERY {
        return Before::Clear;
    }
    let path = pid_path(data_dir, id);
    // A child of this daemon is one of its own workers, never an orphan:
    // after a reboot on Linux, where start times count from the boot, an
    // old record of another worker could name it.
    if let Some(record) = process::read_pid_file(&path)
        && probes
            .children(std::process::id())
            .await
            .is_some_and(|children| children.iter().any(|child| child.pid == record.pid))
    {
        pid_record::remove(&path, id);
        return Before::Clear;
    }
    let owner = Owner {
        id,
        name: "worker",
        pid_file: path,
        port: None,
        recovery: process::WORKER_ORPHAN_RECOVERY,
    };
    let programs = installed_programs(data_dir, platform, "php");
    let looks = pid_record::STOP_LOOKS;
    match pid_record::clear(&owner, probes, &programs, stop, looks).await {
        Ok(Cleared::Nothing | Cleared::Forgotten) => Before::Clear,
        Ok(Cleared::Stopped { pid, signal }) => Before::Note(stopped_line(id, pid, signal)),
        Ok(Cleared::Unknown { pid }) => Before::Note(format!(
            "marsad: run/{id}.pid names pid {pid}, and the system cannot tell whether it is \
             still the worker that an earlier marsad left running, so it got no signal; if it \
             still runs, two workers take this site's jobs, or run its tasks twice, until it \
             ends."
        )),
        Err(Stuck(reason)) => Before::Stuck(format!("marsad: {reason}.")),
    }
}

fn stopped_line(id: ServiceId, pid: u32, signal: Option<Signal>) -> String {
    let how = signal.map_or("it ended before a signal".to_owned(), |signal| {
        format!("it ended after {}", signal.name())
    });
    format!("marsad: stopped the {id} (pid {pid}) that an earlier marsad left running; {how}.")
}

/// The workers with a record in `run/` (`run/queue-shop.pid`). A name that
/// is no worker's id is left out.
pub(super) fn recorded_workers(data_dir: &Path) -> Vec<ServiceId> {
    let Ok(entries) = std::fs::read_dir(DataDir::new(data_dir).run()) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id: ServiceId = name.strip_suffix(".pid")?.parse().ok()?;
            id.is_worker().then_some(id)
        })
        .collect()
}

/// The code of a worker that [`Before::Stuck`] keeps from starting.
pub(super) const STUCK: &str = codes::WORKER_ORPHAN_STUCK;

/// Wraps the handle of a run just spawned, so `run/<id>.pid` follows each
/// spawn of `program`. `lines` was subscribed before the spawn. Where the
/// system recovers no orphans, the handle stays as it is.
pub(super) fn record(
    handle: ServiceHandle,
    lines: broadcast::Receiver<Event>,
    data_dir: &Path,
    program: PathBuf,
    probes: Arc<dyn ProcessProbe>,
) -> ServiceHandle {
    if !process::WORKER_ORPHAN_RECOVERY {
        return handle;
    }
    let id = lock(&handle.runtime).status.id;
    let pid_file = PidFile {
        path: pid_path(data_dir, id),
    };
    watch(handle, lines, pid_file, program, probes)
}

// Only where the daemon recovers orphans at all: elsewhere `clear` reads
// nothing.
#[cfg(test)]
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    use marsa_core::orphan::{ChildProcess, Liveness, PidRecord};
    use marsa_core::ps::StartTime;
    use marsa_core::types::{SiteKey, WorkerKind};
    use tempfile::TempDir;

    use crate::daemon::services::probe::BoxFuture;

    const PID: u32 = 4242;

    /// A process list that always says `now`, and records each signal.
    #[derive(Debug)]
    struct Fake {
        looks: Mutex<Vec<Liveness>>,
        last: Liveness,
        signals: Mutex<Vec<(u32, Signal)>>,
        /// The children of this daemon.
        children: Vec<u32>,
    }

    impl Fake {
        fn says(looks: Vec<Liveness>, last: Liveness) -> Self {
            Self {
                looks: Mutex::new(looks),
                last,
                signals: Mutex::new(Vec::new()),
                children: Vec::new(),
            }
        }

        fn signals(&self) -> Vec<(u32, Signal)> {
            lock(&self.signals).clone()
        }
    }

    impl ProcessProbe for Fake {
        fn real_identity(&self, _pid: u32) -> BoxFuture<'_, Liveness> {
            let mut looks = lock(&self.looks);
            let now = if looks.is_empty() {
                self.last.clone()
            } else {
                looks.remove(0)
            };
            Box::pin(std::future::ready(now))
        }

        fn children(&self, _ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>> {
            let child = |pid: &u32| ChildProcess {
                pid: *pid,
                started: StartTime::Ticks(7),
            };
            Box::pin(std::future::ready(Some(
                self.children.iter().map(child).collect(),
            )))
        }

        fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, std::io::Result<()>> {
            lock(&self.signals).push((pid, signal));
            Box::pin(std::future::ready(Ok(())))
        }
    }

    fn queue() -> ServiceId {
        let shop = SiteKey::new("shop").expect("a valid site name");
        ServiceId::Worker(WorkerKind::Queue, shop)
    }

    /// A data folder with PHP 8.4.1 installed, and the path of its `php`.
    fn data() -> (TempDir, PathBuf) {
        let temp = TempDir::new().unwrap();
        let bin = temp.path().join("runtimes/php/8.4.1/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("php"), "").unwrap();
        let php = std::fs::canonicalize(bin.join("php")).unwrap();
        (temp, php)
    }

    fn alive(exe: &Path, ticks: u64) -> Liveness {
        Liveness::Alive {
            exe: exe.to_path_buf(),
            started: StartTime::Ticks(ticks),
        }
    }

    /// The record of an old run: our `php`, started at tick 7.
    fn recorded(data: &Path, exe: &Path) {
        let record = PidRecord {
            pid: PID,
            exe: exe.to_path_buf(),
            started: StartTime::Ticks(7),
        };
        process::write_pid_file(&pid_path(data, queue()), &record).unwrap();
    }

    async fn run(fake: &Fake, data: &Path) -> Before {
        let stop = StopMethod::term(Duration::from_millis(50));
        let platform = Platform::current();
        clear(fake, data, platform, queue(), &stop).await
    }

    #[tokio::test]
    async fn no_record_is_clear_and_signals_nothing() {
        let (temp, _) = data();
        let fake = Fake::says(Vec::new(), Liveness::Unknown);
        assert_eq!(run(&fake, temp.path()).await, Before::Clear);
        assert!(fake.signals().is_empty());
    }

    #[tokio::test]
    async fn a_record_that_names_no_worker_of_ours_is_deleted_with_no_signal() {
        let (temp, php) = data();
        let other_data = TempDir::new().unwrap();
        let elsewhere = other_data.path().join("runtimes/php/8.4.1/bin/php");
        for now in [
            // Another program at the pid.
            alive(Path::new("/usr/bin/vim"), 7),
            // Our php, but started at another time: the pid changed hands.
            alive(&php, 8),
            alive(&php, 6),
            // The php of another data folder.
            alive(&elsewhere, 7),
            Liveness::Gone,
        ] {
            recorded(temp.path(), &php);
            let fake = Fake::says(Vec::new(), now.clone());
            assert_eq!(run(&fake, temp.path()).await, Before::Clear, "{now:?}");
            assert!(fake.signals().is_empty(), "{now:?}");
            assert!(!pid_path(temp.path(), queue()).exists(), "{now:?}");
        }
    }

    #[tokio::test]
    async fn a_pid_that_cannot_be_told_gets_no_signal_and_the_record_stays() {
        let (temp, php) = data();
        recorded(temp.path(), &php);
        let fake = Fake::says(Vec::new(), Liveness::Unknown);
        let Before::Note(text) = run(&fake, temp.path()).await else {
            panic!("a note");
        };
        assert!(text.contains("cannot tell"), "{text}");
        assert!(fake.signals().is_empty());
        assert!(pid_path(temp.path(), queue()).exists());
    }

    #[tokio::test]
    async fn our_old_worker_gets_sigterm_and_its_record_goes() {
        let (temp, php) = data();
        recorded(temp.path(), &php);
        // Ours at the first two checks (before the start, before SIGTERM),
        // then gone.
        let fake = Fake::says(vec![alive(&php, 7), alive(&php, 7)], Liveness::Gone);
        let before = run(&fake, temp.path()).await;
        let text = format!(
            "marsad: stopped the queue-shop (pid {PID}) that an earlier marsad left running; it \
             ended after SIGTERM."
        );
        assert_eq!(before, Before::Note(text));
        assert_eq!(fake.signals(), [(PID, Signal::Term)]);
        assert!(!pid_path(temp.path(), queue()).exists());
    }

    #[tokio::test]
    async fn our_old_worker_that_outlives_sigkill_keeps_the_new_one_from_starting() {
        let (temp, php) = data();
        recorded(temp.path(), &php);
        let fake = Fake::says(Vec::new(), alive(&php, 7));
        let Before::Stuck(text) = run(&fake, temp.path()).await else {
            panic!("stuck");
        };
        assert!(text.contains("still runs after SIGKILL"), "{text}");
        assert_eq!(fake.signals(), [(PID, Signal::Term), (PID, Signal::Kill)]);
    }

    /// The record and the process agree, but the program is not the PHP CLI
    /// of an installed version of this data folder: not ours.
    #[tokio::test]
    async fn only_the_php_of_this_data_folder_is_ours() {
        let (temp, _) = data();
        let other_data = TempDir::new().unwrap();
        let not_ours = [
            other_data.path().join("runtimes/php/8.4.1/bin/php"),
            temp.path().join("runtimes/php/8.4.1/bin/php-fpm"),
            temp.path().join("runtimes/php/8.3.0/bin/php"),
        ];
        for exe in not_ours {
            recorded(temp.path(), &exe);
            let fake = Fake::says(Vec::new(), alive(&exe, 7));
            assert_eq!(run(&fake, temp.path()).await, Before::Clear, "{exe:?}");
            assert!(fake.signals().is_empty(), "{exe:?}");
            assert!(!pid_path(temp.path(), queue()).exists(), "{exe:?}");
        }
    }

    #[tokio::test]
    async fn a_child_of_this_daemon_is_never_an_orphan() {
        let (temp, php) = data();
        recorded(temp.path(), &php);
        let mut fake = Fake::says(Vec::new(), alive(&php, 7));
        fake.children = vec![PID];
        assert_eq!(run(&fake, temp.path()).await, Before::Clear);
        assert!(fake.signals().is_empty());
        assert!(!pid_path(temp.path(), queue()).exists());
    }

    /// Kills the process at the end of a test, whatever happened.
    struct KillOnDrop(u32);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", &self.0.to_string()])
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    /// A real orphan: `sleep` stands for our PHP, started outside any
    /// supervisor, as a crashed daemon leaves it. Our `php` is a link to it:
    /// macOS kills a copy of a system program, and the check follows the
    /// link (`accepted`).
    #[tokio::test]
    async fn a_real_orphan_is_stopped_and_gone() {
        let (temp, _) = data();
        let link = temp.path().join("runtimes/php/8.4.1/bin/php");
        std::fs::remove_file(&link).unwrap();
        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.exists())
            .unwrap();
        std::os::unix::fs::symlink(sleep, &link).unwrap();
        let php = std::fs::canonicalize(&link).unwrap();
        // Started by a shell that ends at once, so the orphan's parent is not
        // this process, which stands for the daemon.
        let started = std::process::Command::new("/bin/sh")
            .args(["-c", "\"$0\" 600 >/dev/null 2>&1 & echo $!"])
            .arg(&link)
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8(started.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let _kill = KillOnDrop(pid);
        let identity = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Liveness::Alive { exe, started } = process::real_identity(pid).await
                    && exe == php
                {
                    return (exe, started);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let (exe, started) = identity.await.expect("the orphan runs");
        let record = PidRecord { pid, exe, started };
        process::write_pid_file(&pid_path(temp.path(), queue()), &record).unwrap();

        let stop = StopMethod::term(Duration::from_secs(5));
        let platform = Platform::current();
        let probes = crate::daemon::services::probe::SystemProbe;
        let before = clear(&probes, temp.path(), platform, queue(), &stop).await;
        let Before::Note(text) = before else {
            panic!("a note: {before:?}");
        };
        assert!(text.contains(&format!("(pid {pid})")), "{text}");
        assert_eq!(process::real_identity(pid).await, Liveness::Gone);
        assert!(!pid_path(temp.path(), queue()).exists());
    }
}
