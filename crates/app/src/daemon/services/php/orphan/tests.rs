use super::*;
use marsa_core::data_dir::DataDir;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use marsa_core::caddy::Dial;
use marsa_core::orphan::{Liveness, PidRecord};
use marsa_core::php::PhpMinor;
use marsa_core::platform::process::{self, Signal};
use marsa_core::ps::StartTime;
use marsa_core::recipes::Platform;
use marsa_core::types::{Event, ServiceState};
// Only the tests of the systems where php-fpm pools run use it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use marsa_core::types::ServiceId;
use tokio::sync::broadcast::{self};

use super::super::{fpm_id, fpm_program};
use crate::daemon::services::caddy::orphan::Ladder;
use crate::daemon::services::probe::BoxFuture;
use crate::daemon::services::probe::ProcessProbe;
use crate::daemon::services::supervisor::{ServiceHandle, ServiceRuntime, lock};
use marsa_core::orphan::ChildProcess;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;

use semver::Version;
use tokio_util::sync::CancellationToken;

use marsa_core::php::PhpFiles;
use marsa_core::types::ServiceStatus;

use super::super::{PhpManager, Pool, fpm_spec};
use crate::daemon::state::Daemon;

const PROGRAM: &str = "/data/runtimes/php/8.4.3/bin/php-fpm";
/// `Wed Nov 4 03:31:27 2026`.
const STARTED: StartTime = StartTime::Utc(1_793_763_087);
/// A second before and after `STARTED`.
const EARLIER: StartTime = StartTime::Utc(1_793_763_086);
const LATER: StartTime = StartTime::Utc(1_793_763_088);
/// The pid the pid file names.
const MASTER: u32 = 4242;
const PHP84: PhpMinor = PhpMinor { major: 8, minor: 4 };

/// Short waits for the fakes, which change state at once.
const SHORT: Ladder = Ladder {
    stop: Duration::ZERO,
    term: Duration::from_millis(200),
    kill: Duration::from_millis(500),
};

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// One process the fake OS knows.
#[derive(Debug, Clone, Default)]
struct Proc {
    ppid: u32,
    /// What the OS says: the front of the queue, and the last one again
    /// once one is left.
    now: VecDeque<Liveness>,
    /// The start time the process table lists for it: the first `Alive`
    /// of `now`, unless set.
    listed: Option<StartTime>,
    /// What the OS says after a signal, as the death of the process
    /// would. `None`: the signal changes nothing.
    after_term: Option<Liveness>,
    after_kill: Option<Liveness>,
    /// Forked after its parent's children were listed: the process
    /// table shows it only once its parent is gone, under pid 1.
    late: bool,
}

/// A process that says `now`, with no parent of note.
fn proc(now: impl IntoIterator<Item = Liveness>) -> Proc {
    let now: VecDeque<Liveness> = now.into_iter().collect();
    let listed = now.iter().find_map(|state| match state {
        Liveness::Alive { started, .. } => Some(*started),
        _ => None,
    });
    Proc {
        now,
        listed,
        ..Proc::default()
    }
}

/// A child of the master that runs `exe`, started at `started`.
fn child(exe: &str, started: StartTime) -> Proc {
    Proc {
        ppid: MASTER,
        ..proc([alive(exe, started)])
    }
}

/// A master that ignores SIGTERM and dies from SIGKILL.
fn stubborn_master() -> Proc {
    Proc {
        after_kill: Some(Liveness::Gone),
        ..proc([ours()])
    }
}

/// A worker of the master: its program, started at `started`, ends on
/// SIGTERM.
fn worker(started: StartTime) -> Proc {
    Proc {
        after_term: Some(Liveness::Gone),
        ..child(PROGRAM, started)
    }
}

/// The fake OS. A signal is recorded, never sent, and may change what the
/// OS says next, as the death of the process would: its listener closes,
/// and pid 1 adopts its children.
#[derive(Debug, Default)]
struct Fake {
    procs: Mutex<BTreeMap<u32, Proc>>,
    /// The parents whose children cannot be listed (`ps` fails).
    unlisted: Vec<u32>,
    /// `lsof` fails: the holders of the listener cannot be listed.
    no_holders: bool,
    /// How often the holders of the listener were asked for: once per
    /// search under pid 1.
    searches: Mutex<usize>,
    /// The pids that hold the pool's listener, each until a signal ends
    /// it.
    holders: Mutex<BTreeSet<u32>>,
    /// A subreaper that adopts orphans instead of pid 1, as
    /// `systemd --user` does on Linux. It is searched before pid 1.
    subreaper: Option<u32>,
    /// The port they hold, let go with the last of them.
    port: Mutex<Option<TcpListener>>,
    signals: Mutex<Vec<(u32, Signal)>>,
    /// How often the hook before the first signal was called.
    announced: Mutex<usize>,
    /// How often a pid's identity was asked for.
    looks: Mutex<usize>,
}

impl Fake {
    /// The master, pid 4242, says `now`.
    fn new(now: impl IntoIterator<Item = Liveness>) -> Self {
        Self::of(proc(now))
    }

    /// `master` as pid 4242. The daemon that started it is gone, so pid
    /// 1 adopted it, as on macOS: the search under pid 1 lists it too.
    fn of(master: Proc) -> Self {
        Self::default().with(
            MASTER,
            Proc {
                ppid: INIT,
                ..master
            },
        )
    }

    fn with(self, pid: u32, proc: Proc) -> Self {
        self.procs.lock().unwrap().insert(pid, proc);
        self
    }

    /// `pids` hold the pool's listener: `listener`'s port stays in use
    /// until a signal has ended each of them.
    fn holding(self, pids: impl IntoIterator<Item = u32>, listener: TcpListener) -> Self {
        *self.port.lock().unwrap() = Some(listener);
        self.holding_unseen(pids)
    }

    /// `pids` hold the pool's listener, and the probe reads it as free,
    /// as it reads a full backlog on macOS.
    fn holding_unseen(self, pids: impl IntoIterator<Item = u32>) -> Self {
        self.holders.lock().unwrap().extend(pids);
        self
    }

    fn signals(&self) -> Vec<(u32, Signal)> {
        self.signals.lock().unwrap().clone()
    }

    fn announced(&self) -> usize {
        *self.announced.lock().unwrap()
    }

    fn searches(&self) -> usize {
        *self.searches.lock().unwrap()
    }

    fn looks(&self) -> usize {
        *self.looks.lock().unwrap()
    }
}

impl ProcessProbe for Fake {
    fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        *self.looks.lock().unwrap() += 1;
        let mut procs = self.procs.lock().unwrap();
        let next = match procs.get_mut(&pid) {
            Some(proc) => {
                let next = if proc.now.len() > 1 {
                    proc.now.pop_front()
                } else {
                    proc.now.front().cloned()
                };
                next.unwrap_or(Liveness::Unknown)
            }
            None => Liveness::Gone,
        };
        Box::pin(async move { next })
    }

    fn children(&self, ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>> {
        let children = (!self.unlisted.contains(&ppid)).then(|| {
            let procs = self.procs.lock().unwrap();
            procs
                .iter()
                .filter(|(_, proc)| proc.ppid == ppid && !proc.late)
                .map(|(pid, proc)| ChildProcess {
                    pid: *pid,
                    // No time at all compares with nothing.
                    started: proc.listed.unwrap_or(StartTime::Ticks(0)),
                })
                .collect()
        });
        Box::pin(async move { children })
    }

    fn adopters(&self) -> BoxFuture<'_, Vec<u32>> {
        let adopters = self.subreaper.into_iter().chain([INIT]).collect();
        Box::pin(async move { adopters })
    }

    fn listener_holders<'a>(&'a self, _listen: &'a Dial) -> BoxFuture<'a, Option<Vec<u32>>> {
        *self.searches.lock().unwrap() += 1;
        let holders =
            (!self.no_holders).then(|| self.holders.lock().unwrap().iter().copied().collect());
        Box::pin(async move { holders })
    }

    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
        self.signals.lock().unwrap().push((pid, signal));
        let mut procs = self.procs.lock().unwrap();
        let after = procs.get(&pid).and_then(|proc| match signal {
            Signal::Term => proc.after_term.clone(),
            Signal::Kill => proc.after_kill.clone(),
            // The orphan code sends neither.
            Signal::Int | Signal::Quit => None,
        });
        if let Some(state) = after {
            if state == Liveness::Gone {
                // Its listener closes, and pid 1 adopts its children.
                let mut holders = self.holders.lock().unwrap();
                holders.remove(&pid);
                if holders.is_empty() {
                    *self.port.lock().unwrap() = None;
                }
                for orphan in procs.values_mut().filter(|proc| proc.ppid == pid) {
                    orphan.ppid = self.subreaper.unwrap_or(INIT);
                    orphan.late = false;
                }
            }
            if let Some(proc) = procs.get_mut(&pid) {
                proc.now = VecDeque::from([state]);
            }
        }
        Box::pin(async { Ok(()) })
    }
}

fn alive(exe: &str, started: StartTime) -> Liveness {
    Liveness::Alive {
        exe: PathBuf::from(exe),
        started,
    }
}

fn ours() -> Liveness {
    alive(PROGRAM, STARTED)
}

fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// A pid file that names pid 4242, our php-fpm.
fn pid_file_of_ours() -> (tempfile::TempDir, PidFile) {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    let record = PidRecord {
        pid: 4242,
        exe: PathBuf::from(PROGRAM),
        started: STARTED,
    };
    process::write_pid_file(&pid_file.path, &record).unwrap();
    (dir, pid_file)
}

/// The check before a start, as `PhpManager::clear_orphan` runs it.
async fn run(pid_file: &PidFile, fake: &Fake, listen: &Dial) -> Result<(), Blocked> {
    run_with(pid_file, fake, listen, SHORT).await
}

/// [`run`] with other waits. `ladder.term` of zero makes each wait
/// after a SIGTERM ask the OS exactly once.
async fn run_with(
    pid_file: &PidFile,
    fake: &Fake,
    listen: &Dial,
    ladder: Ladder,
) -> Result<(), Blocked> {
    let accepted = [PathBuf::from(PROGRAM)];
    let orphans = Orphans {
        pid_file,
        processes: fake,
        accepted: &accepted,
        listen,
        ladder,
        worker_ladder: ladder,
    };
    let found = orphans.inspect().await?;
    let announce = || *fake.announced.lock().unwrap() += 1;
    orphans.reap(found, &announce).await
}

/// A port something holds: `Dial::Tcp` of it, and the listener to hand
/// to the process that holds it.
fn held_port() -> (TcpListener, Dial) {
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let listen = Dial::Tcp(held.local_addr().unwrap().port());
    (held, listen)
}

/// The 8.4 pool of `data` on `listen`, run as `program args`. Without
/// php-fpm's config text, so it builds from a Windows path too.
fn bare_pool(data: &Path, listen: Dial, program: &str, args: &[&str]) -> Pool {
    let files = PhpFiles::new(data, &v("8.4.3"));
    let mut spec = fpm_spec(PathBuf::from(program), &files);
    spec.args = args.iter().map(|arg| (*arg).to_owned()).collect();
    Pool {
        files,
        fpm_conf: String::new(),
        php_ini: String::new(),
        spec,
        dial: listen,
    }
}

/// A manager that checks for orphans with `processes`, on any system.
fn manager_with(data: &Path, processes: Arc<dyn ProcessProbe>) -> PhpManager {
    let mut php = PhpManager::new(data, Some(Platform::LinuxX64));
    php.recovery = true;
    php.processes = processes;
    php.ladder = SHORT;
    php.worker_ladder = SHORT;
    php
}

#[tokio::test]
async fn a_stubborn_orphan_gets_sigterm_then_sigkill_and_its_file_goes() {
    let (_dir, pid_file) = pid_file_of_ours();
    let fake = Fake::of(stubborn_master());
    let listen = Dial::Tcp(free_port());
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    let want = [(4242, Signal::Term), (4242, Signal::Kill)];
    assert_eq!(fake.signals(), want);
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn an_orphan_that_stops_on_sigterm_gets_no_sigkill() {
    let (_dir, pid_file) = pid_file_of_ours();
    let fake = Fake::of(Proc {
        after_term: Some(Liveness::Gone),
        ..proc([ours()])
    });
    let listen = Dial::Tcp(free_port());
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    assert_eq!(fake.signals(), [(4242, Signal::Term)]);
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn a_socket_let_go_a_moment_after_the_pid_is_gone_does_not_block_the_pool() {
    // On Linux a process reads as gone (no `exe` link, or a zombie)
    // before its last thread has closed its files, so its socket or port
    // stays in use for a moment after SIGTERM ended it. The pool waits
    // for that instead of failing.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let fake = Fake::of(Proc {
        after_term: Some(Liveness::Gone),
        ..proc([ours()])
    });
    let closing = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(held);
    });
    // A long last step, so a slow machine cannot fail the test; the wait
    // ends as soon as the port is free.
    let ladder = Ladder {
        kill: Duration::from_secs(10),
        ..SHORT
    };
    assert_eq!(run_with(&pid_file, &fake, &listen, ladder).await, Ok(()));
    assert_eq!(fake.signals(), [(MASTER, Signal::Term)]);
    assert!(!pid_file.path.exists());
    closing.join().unwrap();
}

#[tokio::test]
async fn the_pid_is_checked_again_before_each_signal() {
    // After SIGTERM the pid runs another program, and the port stays
    // busy: that program gets no SIGKILL.
    let (_dir, pid_file) = pid_file_of_ours();
    let (_held, listen) = held_port();
    let fake = Fake::of(Proc {
        after_term: Some(alive("/bin/sleep", STARTED)),
        ..proc([ours()])
    });
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert!(blocked.reason.contains("is still in use"), "{blocked:?}");
    assert_eq!(fake.signals(), [(4242, Signal::Term)], "no SIGKILL");
}

#[tokio::test]
async fn the_workers_of_a_master_killed_by_sigkill_are_stopped_in_order() {
    // Two workers, one of them on the port. The second ignores SIGTERM
    // too. A child that runs another program is not a worker.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let fake = Fake::of(stubborn_master())
        .with(4301, worker(STARTED))
        .with(
            4302,
            Proc {
                after_kill: Some(Liveness::Gone),
                ..child(PROGRAM, LATER)
            },
        )
        .with(4303, child("/bin/sleep", LATER))
        .holding([MASTER, 4301, 4302], held);
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    let want = [
        (MASTER, Signal::Term),
        (MASTER, Signal::Kill),
        (4301, Signal::Term),
        (4302, Signal::Term),
        (4302, Signal::Kill),
    ];
    assert_eq!(fake.signals(), want);
    assert_eq!(fake.announced(), 1, "once, before the first signal");
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn a_child_that_is_not_the_masters_worker_gets_no_signal() {
    let cases = [
        ("another program", child("/bin/sleep", STARTED)),
        (
            "a php-fpm of another data folder",
            child("/other/runtimes/php/8.4.3/bin/php-fpm", STARTED),
        ),
        ("older than the master", child(PROGRAM, EARLIER)),
        (
            "an identity that cannot be read",
            Proc {
                listed: Some(STARTED),
                ppid: MASTER,
                ..proc([Liveness::Unknown])
            },
        ),
    ];
    for (what, decoy) in cases {
        let (_dir, pid_file) = pid_file_of_ours();
        // The decoy holds the port, so a signal would have been the only
        // way on; the master's SIGKILL alone leaves it in use. Pid 1
        // adopts the decoy, and the search there turns it down again.
        let (held, listen) = held_port();
        let fake = Fake::of(stubborn_master())
            .with(4300, decoy)
            .holding([MASTER, 4300], held);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK, "{what}");
        assert!(blocked.reason.contains("has exited"), "{what}: {blocked:?}");
        let want = [(MASTER, Signal::Term), (MASTER, Signal::Kill)];
        assert_eq!(fake.signals(), want, "{what}");
        assert!(pid_file.path.exists(), "{what}: kept for the next try");
    }
}

#[tokio::test]
async fn a_master_whose_children_cannot_be_listed_gets_no_sigkill() {
    // Killed, it would leave its workers with no record of whose they
    // are. It keeps running, and the file stays for the next try.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let fake = Fake {
        unlisted: vec![MASTER],
        ..Fake::of(stubborn_master())
    }
    .with(4301, worker(STARTED))
    .holding([MASTER, 4301], held);
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert!(blocked.reason.contains("cannot be listed"), "{blocked:?}");
    assert_eq!(fake.signals(), [(MASTER, Signal::Term)], "no SIGKILL");
    assert!(pid_file.path.exists(), "kept for the next try");
}

#[tokio::test]
async fn a_master_gone_at_the_sigkill_step_has_its_listed_workers_stopped() {
    // The master ignores SIGTERM, then exits by itself right before its
    // SIGKILL, without ending its worker. With no wait, the OS is asked
    // about it exactly four times: at the inspection, before SIGTERM,
    // once after it, and before SIGKILL.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let master = proc([ours(), ours(), ours(), Liveness::Gone]);
    let fake = Fake::of(master)
        .with(4301, worker(STARTED))
        .holding([4301], held);
    let ladder = Ladder {
        term: Duration::ZERO,
        ..SHORT
    };
    assert_eq!(run_with(&pid_file, &fake, &listen, ladder).await, Ok(()));
    // Listed as its child a moment before, and stopped by that list:
    // here it never shows under pid 1.
    assert_eq!(
        fake.signals(),
        [(MASTER, Signal::Term), (4301, Signal::Term)]
    );
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn a_master_gone_at_the_sigkill_step_whose_children_were_not_listed_keeps_the_file() {
    // As above, but its children could not be listed a moment before.
    // Its worker still holds the port and does not show under pid 1
    // yet, so the search there, which works, finds nothing. What holds
    // the port cannot be verified: no signal to it, and the file stays.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let master = proc([ours(), ours(), ours(), Liveness::Gone]);
    let fake = Fake {
        unlisted: vec![MASTER],
        ..Fake::of(master)
    }
    .with(4301, worker(STARTED))
    .holding([4301], held);
    let ladder = Ladder {
        term: Duration::ZERO,
        ..SHORT
    };
    let blocked = run_with(&pid_file, &fake, &listen, ladder)
        .await
        .unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
    assert_eq!(fake.signals(), [(MASTER, Signal::Term)]);
    assert!(pid_file.path.exists(), "kept for the next try");
}

#[tokio::test]
async fn a_master_whose_pid_changed_hands_at_the_sigkill_step_has_pid_1_searched() {
    // The master ignores SIGTERM; right before its SIGKILL its pid runs
    // another program. Its children were listed a moment before, but
    // they may be that program's: 4302 runs our php-fpm and started
    // after the master, and gets no signal. The search under pid 1 runs
    // even though the port reads free, and finds 4301, the master's
    // worker, on it.
    let (_dir, pid_file) = pid_file_of_ours();
    let listen = Dial::Tcp(free_port());
    let master = proc([ours(), ours(), ours(), alive("/bin/sleep", LATER)]);
    let adopted = Proc {
        ppid: 1,
        ..worker(LATER)
    };
    let fake = Fake::of(master)
        .with(4301, adopted)
        .with(4302, worker(LATER))
        .holding_unseen([4301]);
    let ladder = Ladder {
        term: Duration::ZERO,
        ..SHORT
    };
    assert_eq!(run_with(&pid_file, &fake, &listen, ladder).await, Ok(()));
    assert_eq!(
        fake.signals(),
        [(MASTER, Signal::Term), (4301, Signal::Term)]
    );
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn a_worker_forked_after_the_list_is_found_under_pid_1_once_the_master_is_killed() {
    // 4301 is listed before the SIGKILL. 4302 is forked after that. Pid 1
    // adopts both, and the search there finds the one the list missed.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let late = Proc {
        late: true,
        ..worker(LATER)
    };
    let fake = Fake::of(stubborn_master())
        .with(4301, worker(STARTED))
        .with(4302, late)
        .holding([MASTER, 4301, 4302], held);
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    let want = [
        (MASTER, Signal::Term),
        (MASTER, Signal::Kill),
        (4301, Signal::Term),
        (4302, Signal::Term),
    ];
    assert_eq!(fake.signals(), want);
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn after_a_sigkill_pid_1_is_searched_even_if_the_port_reads_free() {
    // A full backlog reads as free on macOS. The worker was forked after
    // the master's children were listed, so only the search finds it.
    let (_dir, pid_file) = pid_file_of_ours();
    let listen = Dial::Tcp(free_port());
    let late = Proc {
        late: true,
        ..worker(LATER)
    };
    let fake = Fake::of(stubborn_master())
        .with(4302, late)
        .holding_unseen([4302]);
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    let want = [
        (MASTER, Signal::Term),
        (MASTER, Signal::Kill),
        (4302, Signal::Term),
    ];
    assert_eq!(fake.signals(), want);
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn a_remembered_worker_does_not_get_its_signals_twice() {
    // The worker survives both signals. Pid 1 adopts it, and it still
    // holds the port, but the search there leaves it out: it had its
    // turn.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let fake = Fake::of(stubborn_master())
        .with(4301, child(PROGRAM, STARTED))
        .holding([MASTER, 4301], held);
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    let want = [
        (MASTER, Signal::Term),
        (MASTER, Signal::Kill),
        (4301, Signal::Term),
        (4301, Signal::Kill),
    ];
    assert_eq!(fake.signals(), want);
    assert!(pid_file.path.exists(), "kept for the next try");
}

#[tokio::test]
async fn a_worker_whose_identity_changes_before_sigkill_gets_none() {
    // The worker ignores SIGTERM; right before SIGKILL its pid runs
    // another program. With no wait, the OS is asked exactly four times:
    // listed, before SIGTERM, once after it, and before SIGKILL.
    let (_dir, pid_file) = pid_file_of_ours();
    let listen = Dial::Tcp(free_port());
    let same = alive(PROGRAM, STARTED);
    let changed = Proc {
        ppid: MASTER,
        ..proc([same.clone(), same.clone(), same, alive("/bin/sleep", LATER)])
    };
    let fake = Fake::of(stubborn_master()).with(4300, changed);
    let ladder = Ladder {
        term: Duration::ZERO,
        ..SHORT
    };
    assert_eq!(run_with(&pid_file, &fake, &listen, ladder).await, Ok(()));
    let want = [
        (MASTER, Signal::Term),
        (MASTER, Signal::Kill),
        (4300, Signal::Term),
    ];
    assert_eq!(fake.signals(), want, "no SIGKILL to the worker");
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn workers_under_pid_1_are_stopped_when_the_master_is_already_gone() {
    // The file names a master that is gone; two of its workers live on
    // under pid 1, on the port.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let adopted = |proc: Proc| Proc { ppid: 1, ..proc };
    let fake = Fake::new([Liveness::Gone])
        .with(4301, adopted(worker(STARTED)))
        .with(4302, adopted(worker(LATER)))
        .holding([4301, 4302], held);
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    assert_eq!(fake.signals(), [(4301, Signal::Term), (4302, Signal::Term)]);
    assert_eq!(fake.announced(), 1, "once, before the first signal");
    assert!(!pid_file.path.exists());
}

/// On Linux a subreaper (`systemd --user`) adopts the
/// workers, and one stays under pid 1. Both are searched; a worker of
/// another parent, on the port too, is not.
#[tokio::test]
async fn workers_under_a_subreaper_are_stopped_too() {
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    const SUBREAPER: u32 = 900;
    let under = |ppid: u32, proc: Proc| Proc { ppid, ..proc };
    let fake = Fake {
        subreaper: Some(SUBREAPER),
        ..Fake::new([Liveness::Gone])
    }
    .with(4301, under(SUBREAPER, worker(STARTED)))
    .with(4302, under(INIT, worker(LATER)))
    .with(4303, under(4999, worker(LATER)))
    .holding([4301, 4302, 4303], held);
    let result = run(&pid_file, &fake, &listen).await;
    assert_eq!(fake.signals(), [(4301, Signal::Term), (4302, Signal::Term)]);
    // 4303 still holds the port: what holds it is not ours.
    assert_eq!(result.unwrap_err().code, ORPHAN_STUCK);
}

#[tokio::test]
async fn a_child_of_pid_1_that_does_not_hold_the_listener_gets_no_signal() {
    // ADR 0009, R5e: our very php-fpm, started after the master, under
    // pid 1, as one that the user started by hand would be. It is not on
    // this pool's port, which another process holds.
    let by_hand = || Proc {
        ppid: 1,
        ..worker(LATER)
    };
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let fake = Fake::new([Liveness::Gone])
        .with(4300, by_hand())
        .holding([4400], held);
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    assert!(fake.signals().is_empty());
    assert_eq!(fake.announced(), 0);
    // The port is still in use: the file goes only after a search.
    assert_eq!(fake.searches(), 1);
    assert!(!pid_file.path.exists());

    // Nor after a SIGKILL of the master, when pid 1 is always searched.
    let (_dir, pid_file) = pid_file_of_ours();
    let listen = Dial::Tcp(free_port());
    let fake = Fake::of(stubborn_master()).with(4300, by_hand());
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    let want = [(MASTER, Signal::Term), (MASTER, Signal::Kill)];
    assert_eq!(fake.signals(), want);
}

/// One adopter whose children cannot be listed proves nothing about
/// the others: no signal at all, and the file stays.
#[tokio::test]
async fn a_subreaper_whose_children_cannot_be_listed_stops_every_signal() {
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    const SUBREAPER: u32 = 900;
    let fake = Fake {
        subreaper: Some(SUBREAPER),
        unlisted: vec![SUBREAPER],
        ..Fake::new([Liveness::Gone])
    }
    .with(
        4301,
        Proc {
            ppid: INIT,
            ..worker(LATER)
        },
    )
    .holding([4301], held);
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
    assert!(fake.signals().is_empty());
    assert!(pid_file.path.exists(), "kept for the next try");
}

#[tokio::test]
async fn a_search_under_pid_1_that_cannot_list_ends_stuck_with_the_file_kept() {
    // A worker under pid 1 holds the port, and a tool fails: `ps` for
    // the children, or `lsof` for the holders. Nothing is proven, so
    // nothing is signalled, and nothing is forgotten.
    for (what, unlisted, no_holders) in [("ps", vec![INIT], false), ("lsof", vec![], true)] {
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let adopted = Proc {
            ppid: 1,
            ..worker(LATER)
        };
        let fake = Fake {
            unlisted,
            no_holders,
            ..Fake::new([Liveness::Gone])
        }
        .with(4301, adopted)
        .holding([4301], held);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK, "{what}");
        let unsure = blocked.reason.contains("cannot be verified");
        assert!(unsure, "{what}: {blocked:?}");
        assert!(fake.signals().is_empty(), "{what}");
        assert_eq!(fake.announced(), 0, "{what}");
        assert!(pid_file.path.exists(), "{what}: kept for the next try");
    }
}

#[tokio::test]
async fn a_worker_under_pid_1_that_cannot_be_told_before_its_signal_gets_none() {
    // Verified when it is listed; right before SIGTERM its identity
    // cannot be read. It may still be the worker: no signal, and the
    // file stays.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let unsure = Proc {
        ppid: 1,
        ..proc([alive(PROGRAM, LATER), Liveness::Unknown])
    };
    let fake = Fake::new([Liveness::Gone])
        .with(4301, unsure)
        .holding([4301], held);
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
    assert!(fake.signals().is_empty());
    assert!(pid_file.path.exists(), "kept for the next try");
}

#[tokio::test]
async fn a_child_of_pid_1_that_is_not_the_masters_worker_gets_no_signal() {
    // A second after `LATER`.
    const LATEST: StartTime = StartTime::Utc(1_793_763_089);
    // What the process table lists, and what the OS says right after.
    let cases = [
        (
            "a php-fpm of another data folder",
            LATER,
            alive("/other/runtimes/php/8.4.3/bin/php-fpm", LATER),
        ),
        ("another program", LATER, alive("/bin/sleep", LATER)),
        (
            "our php-fpm from before the master",
            EARLIER,
            alive(PROGRAM, EARLIER),
        ),
        // The listed process exited, and our php-fpm took its pid.
        (
            "a pid that changed hands after the list",
            LATER,
            alive(PROGRAM, LATEST),
        ),
    ];
    for (what, listed, now) in cases {
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let decoy = Proc {
            ppid: 1,
            listed: Some(listed),
            ..proc([now])
        };
        // It holds the port, so only the checks of its executable and
        // start time stand between it and a signal.
        let fake = Fake::new([Liveness::Gone])
            .with(4300, decoy)
            .holding([4300], held);
        // No signal at all, after a search that worked: the pool starts,
        // and php-fpm itself says what holds its port.
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()), "{what}");
        assert!(fake.signals().is_empty(), "{what}");
        assert_eq!(fake.announced(), 0, "{what}");
        assert_eq!(
            fake.searches(),
            1,
            "{what}: the file goes only after a search"
        );
        assert!(!pid_file.path.exists(), "{what}");
    }
}

#[tokio::test]
async fn a_holder_under_pid_1_that_cannot_be_told_keeps_the_pool_waiting() {
    // Under pid 1, on the port, and its identity cannot be read: it may
    // be the worker. No signal, and the file stays.
    let unknown = || Proc {
        ppid: 1,
        listed: Some(LATER),
        ..proc([Liveness::Unknown])
    };
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let fake = Fake::new([Liveness::Gone])
        .with(4300, unknown())
        .holding([4300], held);
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
    assert!(fake.signals().is_empty());
    assert!(pid_file.path.exists(), "kept for the next try");

    // Not on the port: a bystander, which never blocks the pool.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let fake = Fake::new([Liveness::Gone])
        .with(4300, unknown())
        .holding([4400], held);
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    assert!(fake.signals().is_empty());
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn the_masters_own_pid_under_pid_1_is_never_taken_for_its_worker() {
    // The master's pid reads gone at the inspection (a `ps` that missed
    // it), then as our php-fpm again, under pid 1 and on the port, as a
    // live master is. The search there passes it over: the master is
    // judged by the pid file alone. Its worker gets SIGTERM; the master
    // still holds the port, so the pool waits.
    let (_dir, pid_file) = pid_file_of_ours();
    let (held, listen) = held_port();
    let adopted = Proc {
        ppid: 1,
        ..worker(LATER)
    };
    let fake = Fake::new([Liveness::Gone, ours()])
        .with(4301, adopted)
        .holding([MASTER, 4301], held);
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert_eq!(fake.signals(), [(4301, Signal::Term)], "none to the master");
    assert!(pid_file.path.exists(), "kept for the next try");
}

#[tokio::test]
async fn the_stop_is_noted_in_the_pools_lines_only_when_a_signal_goes_out() {
    // Through `PhpManager::clear_orphan`, with an installed php-fpm that
    // the pid file names, so a worker of it counts as ours.
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let program = fpm_program(data, Platform::LinuxX64, &v("8.4.3")).unwrap();
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::write(&program, "not run").unwrap();
    let pid_file = PidFile::new(data, PHP84);
    let write_pid_file = || {
        let record = PidRecord {
            pid: MASTER,
            exe: program.clone(),
            started: STARTED,
        };
        process::write_pid_file(&pid_file.path, &record).unwrap();
    };
    let noted = |daemon: &Daemon| {
        let lines = lock(&daemon.pools().runtime(fpm_id(PHP84), PHP84))
            .logs
            .recent();
        lines.iter().filter(|l| l.text == STOPPING_ORPHAN).count()
    };

    // A stale file, and a port that something of another kind holds:
    // nothing is stopped, so nothing is noted.
    write_pid_file();
    let (held, listen) = held_port();
    let fake = Arc::new(Fake::new([Liveness::Gone]).holding([4400], held));
    let php = manager_with(data, fake.clone());
    let daemon = Daemon::new(data.to_path_buf());
    let pool = bare_pool(data, listen, PROGRAM, &[]);
    let result = php.clear_orphan(&daemon, &pool, &pid_file).await;
    assert_eq!(result, Ok(()));
    assert!(fake.signals().is_empty());
    assert_eq!(noted(&daemon), 0);

    // A worker of the master under pid 1, on the port: noted once.
    write_pid_file();
    let (held, listen) = held_port();
    let worker = Proc {
        ppid: 1,
        after_term: Some(Liveness::Gone),
        ..proc([Liveness::Alive {
            exe: program.clone(),
            started: LATER,
        }])
    };
    let fake = Fake::new([Liveness::Gone])
        .with(4301, worker)
        .holding([4301], held);
    let fake = Arc::new(fake);
    let php = manager_with(data, fake.clone());
    let daemon = Daemon::new(data.to_path_buf());
    let pool = bare_pool(data, listen, PROGRAM, &[]);
    let result = php.clear_orphan(&daemon, &pool, &pid_file).await;
    assert_eq!(result, Ok(()));
    assert_eq!(fake.signals(), [(4301, Signal::Term)]);
    assert_eq!(noted(&daemon), 1);
}

#[tokio::test]
async fn a_pid_file_that_names_no_orphan_of_ours_gets_no_signal() {
    let later = LATER;
    let cases = [
        // Another program, our start time.
        alive("/bin/sleep", STARTED),
        // Our program, another start time: a reused pid (ADR 0009, R5e).
        alive(PROGRAM, later),
        // A php-fpm of another data folder.
        alive("/other/runtimes/php/8.4.3/bin/php-fpm", STARTED),
        Liveness::Gone,
    ];
    for now in cases {
        let (_dir, pid_file) = pid_file_of_ours();
        // Busy, so a signal would have been the only way on.
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen = Dial::Tcp(held.local_addr().unwrap().port());
        let fake = Fake::new([now.clone()]);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()), "{now:?}");
        assert!(fake.signals().is_empty(), "{now:?}");
        assert!(!pid_file.path.exists(), "{now:?}: a stale file goes");
    }
}

#[tokio::test]
async fn a_pid_that_cannot_be_told_blocks_while_busy_and_is_forgotten_once_free() {
    // Busy: blocked, no signal, and the file stays for the next try.
    let (_dir, pid_file) = pid_file_of_ours();
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let listen = Dial::Tcp(held.local_addr().unwrap().port());
    let fake = Fake::new([Liveness::Unknown]);
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
    assert!(fake.signals().is_empty());
    assert!(pid_file.path.exists());

    // Free: the pool may start, and the pid is forgotten.
    drop(held);
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    assert!(fake.signals().is_empty());
    assert!(!pid_file.path.exists());
}

#[tokio::test]
async fn a_blocked_start_pool_fails_with_a_note_and_keeps_the_file() {
    let (dir, pid_file) = pid_file_of_ours();
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let listen = Dial::Tcp(held.local_addr().unwrap().port());
    let fake = Arc::new(Fake::new([Liveness::Unknown]));
    let php = manager_with(dir.path(), fake.clone());
    let daemon = Daemon::new(dir.path().to_path_buf());
    // Never run: the start is blocked first.
    let pool = bare_pool(dir.path(), listen, "/never/run/php-fpm", &[]);

    assert!(!php.start_pool(&daemon, &pool).await);
    let runtime = daemon.pools().find(pool.spec.id).expect("listed");
    let (status, lines) = {
        let runtime = lock(&runtime);
        (runtime.status.clone(), runtime.logs.recent())
    };
    let failed = ServiceState::Failed {
        code: ORPHAN_STUCK.into(),
    };
    assert_eq!((status.state, status.pid), (failed, None));
    let note = "marsad: php-fpm was not started: pid 4242 of the php-fpm pid file";
    assert!(lines.iter().any(|l| l.text.starts_with(note)), "{lines:?}");
    assert!(fake.signals().is_empty());
    assert!(pid_file.path.exists(), "kept for the next try");
    assert!(!daemon.proc_alive(pool.spec.id));
}

// Runs `/bin/sleep` as the pool, which Windows does not have.
#[cfg(unix)]
#[tokio::test]
async fn start_pool_forgets_a_pid_that_cannot_be_told_once_the_port_is_free() {
    let (dir, pid_file) = pid_file_of_ours();
    let fake = Arc::new(Fake::new([Liveness::Unknown]));
    let php = manager_with(dir.path(), fake.clone());
    let daemon = Daemon::new(dir.path().to_path_buf());
    let listen = Dial::Tcp(free_port());
    let pool = bare_pool(dir.path(), listen, "/bin/sleep", &["30"]);

    assert!(php.start_pool(&daemon, &pool).await);
    assert!(daemon.proc_alive(fpm_id(PHP84)));
    assert!(!pid_file.path.exists(), "forgotten");
    assert!(fake.signals().is_empty());
    super::super::stop_pool(&daemon, fpm_id(PHP84)).await;
    assert!(!daemon.proc_alive(fpm_id(PHP84)));
}

#[tokio::test]
async fn a_pid_that_cannot_be_verified_right_before_sigterm_gets_none() {
    // Busy: blocked, and the file stays.
    let (_dir, pid_file) = pid_file_of_ours();
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let listen = Dial::Tcp(held.local_addr().unwrap().port());
    let fake = Fake::new([ours(), Liveness::Unknown]);
    let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
    assert_eq!(blocked.code, ORPHAN_STUCK);
    assert!(fake.signals().is_empty());
    assert!(pid_file.path.exists());

    // Free: the pool may start, and the pid is forgotten.
    drop(held);
    let fake = Fake::new([ours(), Liveness::Unknown]);
    assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
    assert!(fake.signals().is_empty());
    assert!(!pid_file.path.exists());
}

/// What the `exe` link of a child names on Linux until its exec is done:
/// the daemon's own executable, here this test binary.
fn execing(started: StartTime) -> Liveness {
    Liveness::Alive {
        exe: std::env::current_exe().unwrap(),
        started,
    }
}

/// A process this daemon spawned, which says `now`.
fn spawned(now: impl IntoIterator<Item = Liveness>) -> Proc {
    Proc {
        ppid: std::process::id(),
        ..proc(now)
    }
}

#[tokio::test]
async fn the_pid_file_is_written_only_for_our_own_child_that_runs_our_program() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    assert!(
        pid_file
            .path
            .ends_with(Path::new("run").join(format!("{}.pid", fpm_id(PHP84))))
    );
    async fn look(pid_file: &PidFile, fake: Fake, started: Option<StartTime>) -> Recorded {
        pid_file
            .record(&fake, MASTER, Path::new(PROGRAM), started)
            .await
    }
    let child = |now: Liveness| Fake::default().with(MASTER, spawned([now]));
    // The daemon's own executable (the exec is not done yet), or no
    // answer: maybe ours a moment later.
    for now in [execing(STARTED), Liveness::Unknown] {
        let found = look(&pid_file, child(now.clone()), None).await;
        assert_eq!(found, Recorded::NotYet(now));
    }
    // Ours, while the children of the daemon cannot be listed.
    let unlisted = Fake {
        unlisted: vec![std::process::id()],
        ..child(ours())
    };
    let found = look(&pid_file, unlisted, None).await;
    assert_eq!(found, Recorded::NotYet(ours()));
    // Gone; another program; ours under a pid that changed hands since an
    // earlier look; not a child of the daemon; another start time in the
    // process list.
    let not_a_child = Fake::default().with(MASTER, proc([ours()]));
    let listed_later = Fake::default().with(
        MASTER,
        Proc {
            listed: Some(LATER),
            ..spawned([ours()])
        },
    );
    for (fake, started) in [
        (child(Liveness::Gone), None),
        (child(alive("/bin/sleep", STARTED)), None),
        (child(ours()), Some(EARLIER)),
        (not_a_child, None),
        (listed_later, None),
    ] {
        assert_eq!(look(&pid_file, fake, started).await, Recorded::Never);
    }
    assert!(!pid_file.path.exists());

    let want = PidRecord {
        pid: MASTER,
        exe: PathBuf::from(PROGRAM),
        started: STARTED,
    };
    let found = look(&pid_file, child(ours()), Some(STARTED)).await;
    assert_eq!(found, Recorded::Written(want.clone()));
    assert_eq!(process::read_pid_file(&pid_file.path), Some(want));
}

/// A watcher of the 8.4 pool whose supervisor task runs until a stop,
/// and the bus on which a test says what the supervisor would.
fn watched(pid_file: &PidFile, fake: Arc<Fake>) -> (ServiceHandle, broadcast::Sender<Event>) {
    let (events, lines) = broadcast::channel(16);
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let handle = ServiceHandle {
        cancel,
        task: tokio::spawn(async move { stopped.cancelled().await }),
        runtime: Arc::new(Mutex::new(ServiceRuntime::new(fpm_id(PHP84)))),
        stop_limit: Duration::ZERO,
    };
    let program = PathBuf::from(PROGRAM);
    (
        watch(handle, lines, pid_file.clone(), program, fake),
        events,
    )
}

/// What the supervisor says about the 8.4 pool.
fn changed(state: ServiceState, pid: Option<u32>) -> Event {
    Event::ServiceChanged(ServiceStatus {
        id: fpm_id(PHP84),
        state,
        pid,
        restarts: 0,
        since_ms: None,
    })
}

fn running(pid: u32) -> Event {
    changed(ServiceState::Running, Some(pid))
}

fn exited() -> Event {
    let failed = ServiceState::Failed {
        code: "exited".into(),
    };
    changed(failed, None)
}

/// Polls `done` until it holds, for up to 10 s of real time.
async fn soon(what: &str, done: impl Fn() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(10), "{what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Lets the watcher run until `done` holds, without a sleep, so a paused
/// clock does not move.
async fn yielding(what: &str, done: impl Fn() -> bool) {
    for _ in 0..1000 {
        if done() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{what}");
}

/// The pid the pid file names now, if any.
fn named(pid_file: &PidFile) -> Option<u32> {
    process::read_pid_file(&pid_file.path).map(|record| record.pid)
}

#[tokio::test(start_paused = true)]
async fn the_pid_file_is_written_once_the_spawned_pid_runs_our_program() {
    // On Linux, with glibc's `posix_spawn`, `spawn` returns before the
    // exec has switched the child to its program, so for a moment its
    // `exe` link names the daemon. The watcher looks again until it
    // names php-fpm.
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    let now = [execing(STARTED), execing(STARTED), ours()];
    let fake = Arc::new(Fake::default().with(MASTER, spawned(now)));
    let (handle, events) = watched(&pid_file, fake.clone());

    events.send(running(MASTER)).unwrap();
    soon("the pid file", || pid_file.path.exists()).await;
    let want = PidRecord {
        pid: MASTER,
        exe: PathBuf::from(PROGRAM),
        started: STARTED,
    };
    assert_eq!(process::read_pid_file(&pid_file.path), Some(want));
    assert_eq!(fake.looks(), 3);

    events.send(exited()).unwrap();
    soon("no pid file after the exit", || !pid_file.path.exists()).await;
    handle.stop().await.unwrap();
    assert!(fake.signals().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_pid_that_cannot_be_told_at_first_gets_its_file_once_it_can() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    let now = [Liveness::Unknown, ours()];
    let fake = Arc::new(Fake::default().with(MASTER, spawned(now)));
    let (handle, events) = watched(&pid_file, fake.clone());

    events.send(running(MASTER)).unwrap();
    soon("the pid file", || named(&pid_file) == Some(MASTER)).await;
    assert_eq!(fake.looks(), 2);
    handle.stop().await.unwrap();
    assert!(!pid_file.path.exists(), "deleted at the end of the run");
}

#[tokio::test(start_paused = true)]
async fn a_new_spawn_without_an_exit_event_replaces_the_file() {
    // The exit of the first run was missed (a reader too far behind
    // sees only the status now): its file goes before the second one's
    // is written.
    const SECOND: u32 = 4243;
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    let mut second: Vec<Liveness> = vec![execing(LATER); 10];
    second.push(alive(PROGRAM, LATER));
    let fake = Fake::default()
        .with(MASTER, spawned([ours()]))
        .with(SECOND, spawned(second));
    let fake = Arc::new(fake);
    let (handle, events) = watched(&pid_file, fake.clone());

    events.send(running(MASTER)).unwrap();
    soon("the first file", || named(&pid_file) == Some(MASTER)).await;
    events.send(running(SECOND)).unwrap();
    yielding("a look at the second run", || fake.looks() >= 2).await;
    assert_eq!(named(&pid_file), None, "the first file is deleted first");
    soon("the second file", || named(&pid_file) == Some(SECOND)).await;
    let record = process::read_pid_file(&pid_file.path).unwrap();
    assert_eq!(record.started, LATER);
    handle.stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_watcher_gives_up_after_the_record_limit_with_one_warning() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    let fake = Arc::new(Fake::default().with(MASTER, spawned([execing(STARTED)])));
    let (handle, events) = watched(&pid_file, fake.clone());

    events.send(running(MASTER)).unwrap();
    tokio::time::sleep(RECORD_LIMIT + POLL * 4).await;
    // One look at the spawn, then one every `POLL` until the limit, and
    // a last one that may start up to `POLL` after it.
    let looks = fake.looks();
    let most = usize::try_from(RECORD_LIMIT.as_millis() / POLL.as_millis()).unwrap() + 2;
    assert!((most - 3..=most).contains(&looks), "{looks} looks");
    tokio::time::sleep(RECORD_LIMIT).await;
    assert_eq!(fake.looks(), looks, "no look after the limit");
    assert!(!pid_file.path.exists());
    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("php-fpm pid file not written"), "{log}");
    handle.stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_spawned_pid_that_changes_hands_before_it_runs_our_program_gets_no_file() {
    // Our program at the second look, but with another start time: not
    // the process the supervisor spawned. No file, and no more looks.
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    let now = [execing(STARTED), alive(PROGRAM, LATER)];
    let fake = Arc::new(Fake::default().with(MASTER, spawned(now)));
    let (handle, events) = watched(&pid_file, fake.clone());

    events.send(running(MASTER)).unwrap();
    soon("a second look", || fake.looks() >= 2).await;
    tokio::time::sleep(POLL * 10).await;
    assert_eq!(fake.looks(), 2);
    assert!(!pid_file.path.exists());
    handle.stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn another_program_gets_no_file_at_once() {
    // Not the daemon's own executable, so not an exec under way: no more
    // looks (on macOS each look runs `ps` and `lsof`).
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    let now = [alive("/bin/sleep", STARTED)];
    let fake = Arc::new(Fake::default().with(MASTER, spawned(now)));
    let (handle, events) = watched(&pid_file, fake.clone());

    events.send(running(MASTER)).unwrap();
    tokio::time::sleep(POLL * 10).await;
    assert_eq!(fake.looks(), 1);
    assert!(!pid_file.path.exists());
    handle.stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_same_running_again_starts_no_new_look() {
    // A reader too far behind reads the status again, and may see the
    // same `running` twice. The pid is gone at the first look.
    let dir = tempfile::tempdir().unwrap();
    let pid_file = PidFile::new(dir.path(), PHP84);
    let fake = Arc::new(Fake::default());
    let (handle, events) = watched(&pid_file, fake.clone());

    events.send(running(MASTER)).unwrap();
    tokio::time::sleep(POLL * 2).await;
    assert_eq!(fake.looks(), 1);
    events.send(running(MASTER)).unwrap();
    tokio::time::sleep(POLL * 10).await;
    assert_eq!(fake.looks(), 1);
    assert!(!pid_file.path.exists());
    handle.stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_watcher_stops_looking_at_a_stop_and_at_an_exit() {
    let stopping = changed(ServiceState::Stopping, Some(MASTER));
    for end in [stopping, exited()] {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = PidFile::new(dir.path(), PHP84);
        let fake = Arc::new(Fake::default().with(MASTER, spawned([execing(STARTED)])));
        let (handle, events) = watched(&pid_file, fake.clone());

        events.send(running(MASTER)).unwrap();
        soon("a second look", || fake.looks() >= 2).await;
        // The paused clock moves only once the watcher, which runs on
        // this thread, has taken the event in.
        events.send(end.clone()).unwrap();
        let looks = fake.looks();
        tokio::time::sleep(POLL * 10).await;
        assert_eq!(fake.looks(), looks, "{end:?}");
        assert!(!pid_file.path.exists());
        handle.stop().await.unwrap();
    }
}

/// Collects log output in memory.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn without_recovery_a_left_pid_file_goes_without_a_check() {
    let (dir, pid_file) = pid_file_of_ours();
    let fake = Arc::new(Fake::new([ours()]));
    let mut php = PhpManager::new(dir.path(), Some(Platform::LinuxX64));
    php.recovery = false;
    php.processes = fake.clone();
    let daemon = Daemon::new(dir.path().to_path_buf());
    let pool = bare_pool(dir.path(), Dial::Tcp(free_port()), PROGRAM, &[]);
    let result = php.clear_orphan(&daemon, &pool, &pid_file).await;
    assert_eq!(result, Ok(()));
    assert!(fake.signals().is_empty());
    assert!(!pid_file.path.exists());
}

#[test]
fn every_installed_php_fpm_is_accepted_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let platform = Platform::DarwinArm64;
    let mut programs = Vec::new();
    for version in ["8.3.1", "8.4.2", "8.4.3"] {
        let program = fpm_program(dir.path(), platform, &v(version)).unwrap();
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "not run").unwrap();
        programs.push(program);
    }
    // Not a version: never a program.
    let other = DataDir::new(dir.path()).runtimes().join("php").join("8.4");
    std::fs::create_dir_all(other.join("bin")).unwrap();

    let found = installed_programs(dir.path(), platform);
    assert_eq!(found, accepted(programs.clone()));
    for program in &programs {
        assert!(found.contains(program), "{program:?}");
        let real = std::fs::canonicalize(program).unwrap();
        assert!(found.contains(&real), "{real:?}");
    }
    assert!(found.iter().all(|path| !path.starts_with(&other)));
    // Each once.
    let twice = accepted([programs[0].clone(), programs[0].clone()]);
    assert!(twice.len() <= 2, "{twice:?}");
    assert!(installed_programs(&dir.path().join("none"), platform).is_empty());
}

#[tokio::test]
async fn a_port_or_socket_is_free_only_when_nothing_listens() {
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    assert!(!listen_free(&Dial::Tcp(port)).await);
    drop(held);
    assert!(listen_free(&Dial::Tcp(port)).await);

    // Unix sockets exist on Unix only.
    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("php-8.4.sock");
        let socket = Dial::Unix(path.clone());
        assert!(listen_free(&socket).await, "no file");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(!listen_free(&socket).await);
        drop(listener);
        assert!(path.exists());
        assert!(listen_free(&socket).await, "a file nobody listens on");
    }
}

/// The real OS: a fake php-fpm is a copy of this test binary at the
/// exact path of an installed php-fpm, so its real executable is ours.
/// Only the systems where `process::real_identity` reads a process.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod real {
    use super::*;
    use crate::daemon::services::probe::SystemProbe;
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    use std::process::{Child, Command, ExitStatus, Stdio};

    use super::super::super::{pool_for, stop_pool, write_pool};

    /// The fake's mode: `hold` listens on `$MARSA_FPM_SOCKET`, `idle`
    /// does nothing, `once` exits after a second the first time (until
    /// `$MARSA_FPM_MARKER` exists), then does nothing, and `master` runs
    /// a `hold` and an `idle` child, writes their pids to
    /// `$MARSA_FPM_CHILDREN`, and ignores SIGTERM. Each ends by itself
    /// after two minutes.
    const FAKE: &str = "MARSA_FPM_FAKE";
    const SOCKET: &str = "MARSA_FPM_SOCKET";
    /// For `once`: the file that says the first run is over.
    const MARKER: &str = "MARSA_FPM_MARKER";
    /// For `master`: the file with the pids of its children, one per line.
    const CHILDREN: &str = "MARSA_FPM_CHILDREN";
    /// Set for a fake on a leash (`Bystander::leashed`): it ends when its
    /// stdin does, in any mode.
    const LEASH: &str = "MARSA_FPM_LEASH";
    const FAKE_TEST: &str = "daemon::services::php::orphan::tests::real::fake_php_fpm";
    const FAKE_LIFE: Duration = Duration::from_secs(120);
    /// Generous: a test binary can start slowly on a busy CI machine.
    const WAIT: Duration = Duration::from_secs(30);

    /// Not a check: the fake php-fpm's entry point. In a normal test run
    /// the variable is unset and this returns at once.
    #[test]
    fn fake_php_fpm() {
        let Ok(mode) = std::env::var(FAKE) else {
            return;
        };
        if std::env::var_os(LEASH).is_some() {
            // Ends with its input: the test let go of the other end, or
            // is gone.
            std::thread::spawn(|| {
                let _ = io::copy(&mut io::stdin(), &mut io::sink());
                std::process::exit(0);
            });
        }
        if mode == "master" {
            return fake_master();
        }
        let _listener = (mode == "hold").then(|| {
            let path = std::env::var(SOCKET).expect("the socket path");
            std::os::unix::net::UnixListener::bind(path).expect("the socket")
        });
        if mode == "once" {
            let marker = PathBuf::from(std::env::var(MARKER).expect("the marker path"));
            if !marker.exists() {
                std::fs::write(&marker, "").expect("the marker");
                std::thread::sleep(Duration::from_secs(1));
                std::process::exit(1);
            }
        }
        std::thread::sleep(FAKE_LIFE);
    }

    /// A php-fpm whose master only dies from SIGKILL: it catches SIGTERM,
    /// starts two children of this very program, one on the socket as a
    /// worker that serves, and sleeps. The children hold the socket after
    /// its death, as php-fpm's workers do on macOS.
    fn fake_master() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            // A handler, not SIG_IGN: the children start with the default
            // again, as php-fpm's workers do. Nothing reads it; the
            // process just lives on.
            let _term = signal(SignalKind::terminate()).expect("the SIGTERM handler");
            let me = std::env::current_exe().expect("this program");
            let socket = std::env::var(SOCKET).expect("the socket path");
            let pids_file = std::env::var(CHILDREN).expect("the children file");
            let mut children = Vec::new();
            for mode in ["hold", "idle"] {
                let mut command = fake_command(&me, mode);
                // This process's stdin, so the children are on its leash.
                command.env(SOCKET, &socket).stdin(Stdio::inherit());
                children.push(command.spawn().expect("a child"));
            }
            let pids: Vec<String> = children.iter().map(|c| c.id().to_string()).collect();
            std::fs::write(&pids_file, pids.join("\n") + "\n").expect("the children file");
            // Kept, not waited for: they outlive this process.
            tokio::time::sleep(FAKE_LIFE).await;
            drop(children);
        });
    }

    /// Records each signal. Only `send` passes it on to the OS.
    #[derive(Debug, Default)]
    struct Recorder {
        send: bool,
        signals: Mutex<Vec<(u32, Signal)>>,
    }

    impl Recorder {
        fn signals(&self) -> Vec<(u32, Signal)> {
            self.signals.lock().unwrap().clone()
        }
    }

    impl ProcessProbe for Recorder {
        fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
            Box::pin(process::real_identity(pid))
        }

        fn children(&self, ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>> {
            Box::pin(process::children(ppid))
        }

        fn listener_holders<'a>(&'a self, listen: &'a Dial) -> BoxFuture<'a, Option<Vec<u32>>> {
            Box::pin(process::listener_holders(listen))
        }

        fn adopters(&self) -> BoxFuture<'_, Vec<u32>> {
            Box::pin(process::adopters())
        }

        fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
            self.signals.lock().unwrap().push((pid, signal));
            if self.send {
                Box::pin(process::send_signal(pid, signal))
            } else {
                Box::pin(async { Ok(()) })
            }
        }
    }

    /// A process this test started. Dropping it kills it, so its pid
    /// cannot be reused before the `wait`.
    struct Bystander(Child);

    impl Bystander {
        fn spawn(command: &mut Command) -> Self {
            Self(command.stdin(Stdio::null()).spawn().expect("a process"))
        }

        /// Like `spawn`, on a leash: its stdin is a pipe that its
        /// children inherit, and each of them ends when the other end
        /// closes. So none outlives the test, whatever fails in it: that
        /// end is dropped with the test, and closed if the test process
        /// dies. Nothing has to signal a child of the fake by pid.
        fn leashed(command: &mut Command) -> (Self, io::PipeWriter) {
            let (stdin, leash) = io::pipe().expect("a pipe");
            let child = command.env(LEASH, "1").stdin(stdin).spawn();
            (Self(child.expect("a process")), leash)
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }

        fn alive(&mut self) -> bool {
            self.0.try_wait().unwrap().is_none()
        }

        fn wait(&mut self) -> ExitStatus {
            self.0.wait().unwrap()
        }
    }

    impl Drop for Bystander {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn platform() -> Platform {
        Platform::current().expect("Marsa has builds for this system")
    }

    /// `version`'s php-fpm under `data`: a copy of this test binary.
    fn install_fake(data: &Path, version: &str) -> PathBuf {
        let program = placeholder(data, version);
        std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
        program
    }

    /// `version`'s php-fpm under `data`: a file that never runs.
    fn placeholder(data: &Path, version: &str) -> PathBuf {
        let program = fpm_program(data, platform(), &v(version)).unwrap();
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "not run").unwrap();
        program
    }

    fn fake_command(program: &Path, mode: &str) -> Command {
        let mut command = Command::new(program);
        command
            .args([FAKE_TEST, "--exact", "--nocapture"])
            .env(FAKE, mode)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    /// The 8.4 pool of `version`, with its files, run as the fake.
    fn fake_pool(data: &Path, version: &str, mode: &str) -> Pool {
        let mut pool = pool_for(data, platform(), &v(version), None).unwrap();
        write_pool(&pool).unwrap();
        pool.spec.args = [FAKE_TEST, "--exact", "--nocapture"]
            .map(String::from)
            .to_vec();
        pool.spec.env = BTreeMap::from([(FAKE.to_owned(), mode.to_owned())]);
        pool
    }

    fn manager(data: &Path, processes: Arc<dyn ProcessProbe>) -> PhpManager {
        let mut php = PhpManager::new(data, Some(platform()));
        php.recovery = true;
        php.processes = processes;
        php.ladder = Ladder {
            stop: Duration::ZERO,
            term: WAIT,
            kill: Duration::from_secs(5),
        };
        php.worker_ladder = php.ladder;
        php
    }

    async fn wait_until(what: &str, mut done: impl AsyncFnMut() -> bool) {
        let started = Instant::now();
        while !done().await {
            assert!(started.elapsed() < WAIT, "{what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Starts `pool`, or fails with what tells why it did not start: the
    /// pool's status, its recent lines (the reason of the orphan check
    /// is one of them), the signals sent, whether the socket is in use
    /// now, and what the OS says about each of `pids` now.
    async fn start(
        php: &PhpManager,
        daemon: &Daemon,
        pool: &Pool,
        recorder: &Recorder,
        pids: &[u32],
    ) {
        if php.start_pool(daemon, pool).await {
            return;
        }
        let (status, lines) = match daemon
            .pools()
            .find("php-fpm-8.4".parse::<ServiceId>().unwrap())
        {
            Some(runtime) => {
                let runtime = lock(&runtime);
                let lines: Vec<String> =
                    runtime.logs.recent().into_iter().map(|l| l.text).collect();
                (format!("{:?}", runtime.status), lines)
            }
            None => ("no status".to_owned(), Vec::new()),
        };
        let free = listen_free(&pool.dial).await;
        let mut now = Vec::new();
        for pid in pids {
            now.push((*pid, process::real_identity(*pid).await));
        }
        panic!(
            "the pool did not start\nstatus: {status}\nlines: {lines:#?}\nsignals: {:?}\n\
                 socket free now: {free}\nprocesses now: {now:?}",
            recorder.signals()
        );
    }

    /// Runs the check before a start of the 8.4 pool of `data`, with
    /// `record` in the pid file, signals recorded and none sent.
    async fn check_recording(data: &Path, record: &PidRecord) -> Vec<(u32, Signal)> {
        let pid_file = PidFile::new(data, PHP84);
        process::write_pid_file(&pid_file.path, record).unwrap();
        let recorder = Arc::new(Recorder::default());
        let php = manager(data, recorder.clone());
        let daemon = Daemon::new(data.to_path_buf());
        let pool = pool_for(data, platform(), &v("8.4.3"), None).unwrap();
        let result = php.clear_orphan(&daemon, &pool, &pid_file).await;
        assert_eq!(result, Ok(()));
        // Not ours, so the file was stale.
        assert!(!pid_file.path.exists());
        recorder.signals()
    }

    #[tokio::test]
    async fn the_watcher_deletes_the_file_at_an_exit_and_writes_it_after_the_restart() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        install_fake(data, "8.4.3");
        let recorder = Arc::new(Recorder::default());
        let php = manager(data, recorder.clone());
        let daemon = Daemon::new(data.to_path_buf());
        let mut pool = fake_pool(data, "8.4.3", "once");
        let marker = data.join("first-run-over");
        let marker = marker.to_string_lossy().into_owned();
        pool.spec.env.insert(MARKER.to_owned(), marker);
        let pid_file = PidFile::new(data, PHP84);
        let read = || process::read_pid_file(&pid_file.path);

        start(&php, &daemon, &pool, &recorder, &[]).await;
        let mut first = None;
        wait_until("the first pid file", async || {
            first = read();
            first.is_some()
        })
        .await;
        let first = first.expect("a record");
        // The first run exits by itself: its file goes.
        wait_until("no file after the exit", async || read().is_none()).await;
        // The supervisor's restart gets a file of its own.
        wait_until("the file of the restart", async || {
            read().is_some_and(|record| record.pid != first.pid)
        })
        .await;
        let second = read().expect("a record");
        let runtime = daemon.pools().find(fpm_id(PHP84)).expect("listed");
        let status = lock(&runtime).status.clone();
        assert_eq!((status.pid, status.restarts), (Some(second.pid), 1));

        stop_pool(&daemon, fpm_id(PHP84)).await;
        assert_eq!(read(), None);
        assert!(recorder.signals().is_empty());
    }

    #[tokio::test]
    async fn our_orphan_is_stopped_then_the_pool_starts_with_a_new_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        // The orphan runs the older 8.4.2, as after an update.
        let old = install_fake(data, "8.4.2");
        let new = install_fake(data, "8.4.3");
        let recorder = Arc::new(Recorder {
            send: true,
            ..Recorder::default()
        });
        let php = manager(data, recorder.clone());
        let daemon = Daemon::new(data.to_path_buf());
        let pool = fake_pool(data, "8.4.3", "idle");
        let Dial::Unix(socket) = &pool.dial else {
            panic!("a short socket path: {:?}", pool.dial);
        };

        // As a crashed daemon leaves it: on the pool's socket, with a
        // pid file.
        let mut hold = fake_command(&old, "hold");
        let mut orphan = Bystander::spawn(hold.env(SOCKET, socket));
        wait_until("the orphan listens", async || {
            !listen_free(&pool.dial).await
        })
        .await;
        let pid_file = PidFile::new(data, PHP84);
        let recorded = pid_file
            .record(&SystemProbe, orphan.pid(), &old, None)
            .await;
        assert!(matches!(recorded, Recorded::Written(_)), "{recorded:?}");
        let record = process::read_pid_file(&pid_file.path).expect("the orphan's file");
        assert_eq!(record.pid, orphan.pid());

        start(&php, &daemon, &pool, &recorder, &[orphan.pid()]).await;
        // Only SIGTERM, only to the orphan, which it ended.
        assert_eq!(recorder.signals(), [(orphan.pid(), Signal::Term)]);
        assert_eq!(orphan.wait().signal(), Some(15));
        let runtime = daemon.pools().find(fpm_id(PHP84)).expect("listed");
        let lines = lock(&runtime).logs.recent();
        assert!(lines.iter().any(|line| line.text == STOPPING_ORPHAN));

        // The new pool runs, and its pid file names it.
        let mut pid = None;
        wait_until("the pool runs", async || {
            let status = lock(&runtime).status.clone();
            pid = status.pid.filter(|_| status.state == ServiceState::Running);
            pid.is_some()
        })
        .await;
        let pid = pid.expect("a pid");
        wait_until("the new pid file", async || {
            process::read_pid_file(&pid_file.path).is_some_and(|r| r.pid == pid)
        })
        .await;
        let record = process::read_pid_file(&pid_file.path).unwrap();
        assert!(accepted([new]).contains(&record.exe), "{record:?}");

        // A clean stop deletes it.
        stop_pool(&daemon, fpm_id(PHP84)).await;
        assert!(!pid_file.path.exists());
        assert_eq!(lock(&runtime).status.state, ServiceState::Stopped);
    }

    #[tokio::test]
    async fn a_pid_file_naming_a_live_sleep_gets_no_signal() {
        // R4: the pid now runs `sleep`. The file has its start time, so
        // only the path tells it apart.
        let dir = tempfile::tempdir().unwrap();
        let program = placeholder(dir.path(), "8.4.3");
        let mut sleeper = Bystander::spawn(Command::new("/bin/sleep").arg("120"));
        let Liveness::Alive { started, .. } = process::real_identity(sleeper.pid()).await else {
            panic!("no identity for the sleeper");
        };
        let record = PidRecord {
            pid: sleeper.pid(),
            exe: std::fs::canonicalize(&program).unwrap(),
            started,
        };
        assert!(check_recording(dir.path(), &record).await.is_empty());
        assert!(sleeper.alive());
    }

    #[tokio::test]
    async fn our_program_with_another_start_time_gets_no_signal() {
        // R5e: the pid runs our very php-fpm, but not the one we started.
        let dir = tempfile::tempdir().unwrap();
        let program = install_fake(dir.path(), "8.4.3");
        let mut idle = Bystander::spawn(&mut fake_command(&program, "idle"));
        // Only the time differs. On Linux, with glibc's `posix_spawn`,
        // the `exe` link names this test binary until the exec is done.
        let spellings = accepted([program]);
        let mut exe = None;
        wait_until("the idle php-fpm runs our program", async || {
            exe = match process::real_identity(idle.pid()).await {
                Liveness::Alive { exe, .. } if spellings.contains(&exe) => Some(exe),
                _ => None,
            };
            exe.is_some()
        })
        .await;
        let exe = exe.expect("our program");
        let record = PidRecord {
            pid: idle.pid(),
            exe,
            started: StartTime::Utc(0),
        };
        assert!(check_recording(dir.path(), &record).await.is_empty());
        assert!(idle.alive());
    }

    #[tokio::test]
    async fn a_process_that_only_claims_our_path_gets_no_signal() {
        // `argv[0]` is chosen by whoever starts a process: `ps` shows our
        // path for this `sleep` on macOS. Its real executable is not ours.
        let dir = tempfile::tempdir().unwrap();
        let program = placeholder(dir.path(), "8.4.3");
        let mut sleeper = Bystander::spawn(Command::new("/bin/sleep").arg0(&program).arg("120"));
        // `ps` reads `argv[0]` on macOS only; Linux reads the `exe` link.
        #[cfg(target_os = "macos")]
        {
            let claimed = process::identity(sleeper.pid()).await;
            assert!(matches!(&claimed, Liveness::Alive { exe, .. } if *exe == program));
        }
        let Liveness::Alive { started, .. } = process::real_identity(sleeper.pid()).await else {
            panic!("no identity for the sleeper");
        };
        let record = PidRecord {
            pid: sleeper.pid(),
            exe: std::fs::canonicalize(&program).unwrap(),
            started,
        };
        assert!(check_recording(dir.path(), &record).await.is_empty());
        assert!(sleeper.alive());
    }

    #[tokio::test]
    async fn a_dead_pid_gets_no_signal() {
        // R3: the file names a process that has exited.
        let dir = tempfile::tempdir().unwrap();
        let program = placeholder(dir.path(), "8.4.3");
        let mut gone = Command::new("/bin/sleep").arg("0").spawn().unwrap();
        gone.wait().unwrap();
        let record = PidRecord {
            pid: gone.id(),
            exe: std::fs::canonicalize(&program).unwrap(),
            started: STARTED,
        };
        assert!(check_recording(dir.path(), &record).await.is_empty());
    }

    #[tokio::test]
    async fn the_workers_of_a_master_that_only_dies_from_sigkill_are_stopped_too() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let program = install_fake(data, "8.4.3");
        let recorder = Arc::new(Recorder {
            send: true,
            ..Recorder::default()
        });
        let mut php = manager(data, recorder.clone());
        // The master ignores SIGTERM, so that step waits its whole limit:
        // short. Every other wait ends early, the workers' too: theirs
        // stay long (`manager`), so a slow machine cannot turn a
        // worker's SIGTERM into a SIGKILL.
        php.ladder = Ladder {
            stop: Duration::ZERO,
            term: Duration::from_secs(1),
            kill: Duration::from_secs(10),
        };
        let daemon = Daemon::new(data.to_path_buf());
        let pool = fake_pool(data, "8.4.3", "idle");
        let Dial::Unix(socket) = &pool.dial else {
            panic!("a short socket path: {:?}", pool.dial);
        };

        // As a crashed daemon leaves it: a master with two workers, one of
        // them on the pool's socket, and a pid file.
        let pids_file = data.join("children");
        let mut command = fake_command(&program, "master");
        command.env(SOCKET, socket).env(CHILDREN, &pids_file);
        // On a leash, so the workers end even if a check below fails.
        let (mut master, _leash) = Bystander::leashed(&mut command);
        let mut workers: Vec<u32> = Vec::new();
        wait_until("the master's children", async || {
            let text = std::fs::read_to_string(&pids_file).unwrap_or_default();
            workers = text.lines().filter_map(|line| line.parse().ok()).collect();
            workers.len() == 2
        })
        .await;
        workers.sort_unstable();
        wait_until("a worker listens", async || !listen_free(&pool.dial).await).await;
        // Both are listed under the master and run its program.
        let listed = process::children(master.pid()).await.expect("the table");
        for pid in &workers {
            assert!(listed.iter().any(|c| c.pid == *pid), "{pid} in {listed:?}");
            let Liveness::Alive { exe, .. } = process::real_identity(*pid).await else {
                panic!("no identity for worker {pid}");
            };
            assert!(accepted([program.clone()]).contains(&exe), "{exe:?}");
        }
        let pid_file = PidFile::new(data, PHP84);
        let recorded = pid_file
            .record(&SystemProbe, master.pid(), &program, None)
            .await;
        assert!(matches!(recorded, Recorded::Written(_)), "{recorded:?}");

        let watched = [&[master.pid()][..], &workers].concat();
        start(&php, &daemon, &pool, &recorder, &watched).await;
        // SIGTERM did nothing and SIGKILL ended the master; then each
        // worker got SIGTERM, which ended it. Nothing else was signalled.
        assert_eq!(master.wait().signal(), Some(9));
        let mut want = vec![(master.pid(), Signal::Term), (master.pid(), Signal::Kill)];
        want.extend(workers.iter().map(|pid| (*pid, Signal::Term)));
        assert_eq!(recorder.signals(), want);
        for pid in &workers {
            assert_eq!(process::real_identity(*pid).await, Liveness::Gone, "{pid}");
        }
        let runtime = daemon
            .pools()
            .find("php-fpm-8.4".parse::<ServiceId>().unwrap())
            .expect("listed");
        let lines = lock(&runtime).logs.recent();
        assert!(lines.iter().any(|line| line.text == STOPPING_ORPHAN));

        // The new pool runs, and its pid file names it.
        let mut pid = None;
        wait_until("the pool runs", async || {
            let status = lock(&runtime).status.clone();
            pid = status.pid.filter(|_| status.state == ServiceState::Running);
            pid.is_some()
        })
        .await;
        let pid = pid.expect("a pid");
        wait_until("the new pid file", async || {
            process::read_pid_file(&pid_file.path).is_some_and(|r| r.pid == pid)
        })
        .await;
        stop_pool(&daemon, fpm_id(PHP84)).await;
        assert!(!pid_file.path.exists());
        assert_eq!(lock(&runtime).status.state, ServiceState::Stopped);
    }

    /// "Pid 1" is each of `process::adopters`: on Linux the nearest
    /// subreaper may adopt the orphans.
    #[tokio::test]
    async fn under_pid_1_only_the_worker_on_the_pools_socket_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let program = install_fake(data, "8.4.3");
        let recorder = Arc::new(Recorder {
            send: true,
            ..Recorder::default()
        });
        let php = manager(data, recorder.clone());
        let daemon = Daemon::new(data.to_path_buf());
        let pool = fake_pool(data, "8.4.3", "idle");
        let Dial::Unix(socket) = &pool.dial else {
            panic!("a short socket path: {:?}", pool.dial);
        };

        // A master with two children of its program. The first is on the
        // pool's socket: a worker. The second is not: it stands for a
        // php-fpm of this data folder that someone started by hand (ADR
        // 0009, R5e). Both are on a leash, which ends the second one.
        let pids_file = data.join("children");
        let mut command = fake_command(&program, "master");
        command.env(SOCKET, socket).env(CHILDREN, &pids_file);
        let (master, leash) = Bystander::leashed(&mut command);
        let mut children: Vec<u32> = Vec::new();
        wait_until("the master's children", async || {
            let text = std::fs::read_to_string(&pids_file).unwrap_or_default();
            children = text.lines().filter_map(|line| line.parse().ok()).collect();
            children.len() == 2
        })
        .await;
        let (on_socket, by_hand) = (children[0], children[1]);
        wait_until("a worker listens", async || !listen_free(&pool.dial).await).await;
        let pid_file = PidFile::new(data, PHP84);
        let recorded = pid_file
            .record(&SystemProbe, master.pid(), &program, None)
            .await;
        assert!(matches!(recorded, Recorded::Written(_)), "{recorded:?}");

        // The master dies as from a SIGKILL, and pid 1, or a subreaper
        // on Linux, adopts both.
        drop(master);
        let adopters = process::adopters().await;
        wait_until("an adopter adopts both", async || {
            let mut adopted = Vec::new();
            for adopter in &adopters {
                adopted.extend(process::children(*adopter).await.unwrap_or_default());
            }
            let under_init = |pid: &u32| adopted.iter().any(|c| c.pid == *pid);
            children.iter().all(under_init)
        })
        .await;

        start(&php, &daemon, &pool, &recorder, &[on_socket, by_hand]).await;
        // Only the one on the socket, which SIGTERM ended.
        assert_eq!(recorder.signals(), [(on_socket, Signal::Term)]);
        assert_eq!(process::real_identity(on_socket).await, Liveness::Gone);
        // The same program, a later start, under pid 1, and not on the
        // socket: still there.
        let Liveness::Alive { exe, .. } = process::real_identity(by_hand).await else {
            panic!("the php-fpm that is not on the socket was stopped");
        };
        assert!(accepted([program.clone()]).contains(&exe), "{exe:?}");
        let runtime = daemon
            .pools()
            .find("php-fpm-8.4".parse::<ServiceId>().unwrap())
            .expect("listed");
        let lines = lock(&runtime).logs.recent();
        assert!(lines.iter().any(|line| line.text == STOPPING_ORPHAN));

        // The new pool runs, and its pid file names it.
        let mut pid = None;
        wait_until("the pool runs", async || {
            let status = lock(&runtime).status.clone();
            pid = status.pid.filter(|_| status.state == ServiceState::Running);
            pid.is_some()
        })
        .await;
        let pid = pid.expect("a pid");
        wait_until("the new pid file", async || {
            process::read_pid_file(&pid_file.path).is_some_and(|r| r.pid == pid)
        })
        .await;
        stop_pool(&daemon, fpm_id(PHP84)).await;
        assert!(!pid_file.path.exists());

        // Off the leash, the other one ends by itself: no process is left.
        drop(leash);
        wait_until("the other php-fpm ends", async || {
            process::real_identity(by_hand).await == Liveness::Gone
        })
        .await;
    }
}
