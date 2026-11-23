//! Finds and stops a php-fpm that a crashed `marsad` left running,
//! with the rules of ADR 0009 that `caddy::orphan` follows for Caddy.
//!
//! After a SIGKILL of the daemon, php-fpm lives on and keeps its socket, so
//! the next php-fpm of the same PHP line fails ("Another FPM instance seems
//! to already listen") and the line's sites answer 502. So:
//!
//! - after each spawn of a pool, its [`watch`]er writes
//!   `run/php-fpm-<minor>.pid` (pid, real executable, start time) once the
//!   new process runs php-fpm and the process list shows it as a direct
//!   child of the daemon with that start time, and deletes it after each
//!   exit. It looks again for a moment ([`RECORD_LIMIT`]) while the process
//!   still runs the daemon's own executable, as on Linux right after `spawn`
//!   ([`Recorded::NotYet`]), and stops at a change of its start time: the
//!   pid changed hands;
//! - before a pool starts, [`Orphans`] reads that file. A pid counts as ours
//!   only if it is alive, really runs the php-fpm of an installed PHP version
//!   of this data folder (an older version counts: an update may have come in
//!   between), and started at the recorded time. It gets SIGTERM, then
//!   SIGKILL after a limit, each only after the same check passes again.
//!   The pool starts once the process is gone and its socket or port is
//!   free.
//! - php-fpm's workers end with a master that stops on SIGTERM. A master
//!   that only dies from SIGKILL cannot end them, and macOS has no
//!   parent-death signal, so they live on under pid 1 and keep the socket.
//!   So, right before the master's SIGKILL, its direct
//!   children are listed (`ps`), and the ones that run the master's very
//!   executable and started no earlier than it are remembered
//!   ([`orphan::worker_of`]). If they cannot be listed, the master gets no
//!   SIGKILL: its workers would live on with no record of whose they are.
//!   Once the master is gone, each remembered worker gets SIGTERM, then
//!   SIGKILL after a limit, each only right after the same check passes
//!   again (same executable, same start time).
//! - After a SIGKILL of the master, after its pid changed hands, and when
//!   the socket or port is still in use once the master is gone, the
//!   children of pid 1 are searched too: a worker forked after the list was
//!   made is there. Under pid 1 the executable and the start time prove too
//!   little: a php-fpm of this data folder that the user started by hand
//!   sits there as well (ADR 0009, R5e). So a child of pid 1 must also hold
//!   a listener at this pool's socket path or port (`lsof` on macOS,
//!   `/proc` on Linux). That is a filter, not a proof: it shows who holds a
//!   listener there, not who
//!   inherited it from our master. A child of pid 1 that passes it gets the
//!   same two signals after the same checks.
//! - Anything else is never signalled. A stale file is deleted, and the log
//!   says why. What cannot be told is never signalled either: a pid whose
//!   identity cannot be read, or a list that cannot be read (a tool
//!   failed). Then the pool waits, and the file stays, while the socket or
//!   port is in use; once it is free, the file goes and the pool starts.
//!   When a search that worked finds nothing of ours and no signal went
//!   out, the file goes and the pool starts: php-fpm itself then says what
//!   holds its socket.
//!
//! Known limits:
//!
//! - A process whose identity cannot be read at the moment it is listed is
//!   taken for a bystander, since most are, unless it holds the pool's
//!   listener. It gets no signal and does not block the pool; if it was a
//!   worker, php-fpm says that its socket is in use. A worker that was
//!   verified and cannot be read later does block the pool, with
//!   `php_fpm_orphan_stuck`, and so does a holder of the listener under
//!   pid 1 that cannot be read.
//! - The listener filter cannot tell our worker from a php-fpm of this data
//!   folder, under pid 1 and started after our master, that listens at the
//!   very same place: one that bound this pool's socket path itself, or one
//!   on the TCP fallback port, which is not private to Marsa. Such a
//!   process gets the signals of a worker.
//! - Linux: workers under a subreaper that is not an ancestor of this
//!   daemon are not found. The daemon starts detached, so its ancestors can
//!   differ from those of the daemon that crashed (started from a terminal
//!   once, from the tray the next time). The search works and finds
//!   nothing, so the file goes and php-fpm says that its socket is in use.
//! - Linux: `/proc/net/unix` prints socket paths as they are, so another
//!   user can bind a path with a newline that reads as one more line. A
//!   forged line that repeats a real socket's inode is dropped
//!   ([`orphan::parse_proc_net_unix`]); one that repeats our own
//!   listener's makes the holders unknown, so the pool waits with
//!   `php_fpm_orphan_stuck` and nothing is signalled. One that names the
//!   inode of a TCP socket of this user may add its holder to the list.
//!   That holder still needs every check of a worker.
//!
//! On Linux (ADR 0025) an orphan goes to its nearest subreaper, for example
//! `systemd --user`, not always to pid 1. So "pid 1" above means each of
//! [`process::adopters`]: pid 1 on macOS, and on Linux the ancestors of this
//! daemon up to pid 1. Most of those adopt nothing (a shell, the tray);
//! their children are searched with the same checks, so a php-fpm of this
//! data folder that the user started by hand under one of them, on this
//! pool's socket or port, is the known limit above. The holders of a
//! listener come from `/proc` there ([`process::listener_holders`]).
//!
//! php-fpm writes its own `argv[0]` (`php-fpm: master process (...)`), so
//! `ps` shows no path for it. Identity is the file it really runs instead
//! ([`process::real_identity`]: `lsof` on macOS, `/proc` on Linux).
//!
//! All of this runs only where `process::FPM_ORPHAN_RECOVERY` is on (macOS
//! and Linux).
//! Elsewhere no pid file is written, nothing is signalled, and a file left
//! from before is deleted.

use std::time::Duration;

use marsa_core::codes;

/// Our old php-fpm did not stop, or cannot be verified, while its socket or
/// port stays in use.
pub const ORPHAN_STUCK: &str = codes::PHP_FPM_ORPHAN_STUCK;

/// The line before a stop, in the pool's recent lines.
pub const STOPPING_ORPHAN: &str = "marsad: Stopping an old php-fpm left by a crashed marsad.";

/// How often a wait checks again.
const POLL: Duration = Duration::from_millis(50);

/// Pid 1 (`launchd` on macOS), which adopts a process whose parent died.
/// Only the tests' fake OS uses it: the code asks [`ProcessProbe::adopters`].
#[cfg(test)]
const INIT: u32 = 1;

mod find;
mod listen;
mod pid_file;
mod watch;

pub use self::find::{Blocked, Found, Orphans};
pub use self::listen::listen_free;
pub(crate) use self::pid_file::own_executable;
pub use self::pid_file::{PidFile, RECORD_LIMIT, Recorded, accepted, installed_programs};
pub use self::watch::watch;

#[cfg(test)]
mod tests;
