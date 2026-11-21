//! Stopping a managed process, the options every child of `marsad` gets, and
//! what the daemon needs to find an orphan Caddy or php-fpm (ADR 0009): who
//! runs under a pid, its real executable, who listens on a port, signals, the
//! pid files in `run/`, and the data folder's instance id. Also what a client
//! needs to tell a daemon left from an earlier login session,
//! and the parent process that `marsad` logs at start-up.
//!
//! The `#[cfg]` for these lives here, so the supervisor has none. Every tool
//! runs with an absolute path and an argument array, never through a shell.
//!
//! One file for each job: `signals`, `identity`, `login`, `listeners`,
//! and `pid_files`. Their items are re-exported here, so callers use
//! `platform::process::<item>`.

use tokio::process::Command;

#[cfg(target_os = "macos")]
use std::process::Stdio;

#[cfg(unix)]
use std::time::Duration;

mod identity;
mod listeners;
mod login;
mod pid_files;
mod signals;

pub use identity::{ParentProcess, children, executable, identity, parent_process, real_identity};
pub use listeners::{adopters, listener_holders, port_lookup, port_owner, udp_port_owner};
pub use login::{LoginTimes, login_session_start, login_times, read_login_times};
pub use pid_files::{instance_id, read_pid_file, read_small_file, remove_pid_file, write_pid_file};
pub use signals::send_signal;

/// Whether the daemon writes `run/caddy.pid` after each start of Caddy, and
/// looks for an orphan Caddy before it starts one: macOS (ADR 0009) and
/// Linux (ADR 0025). On Windows a Job Object stops Caddy with
/// the daemon (ADR 0017).
pub const CADDY_ORPHAN_RECOVERY: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// The same for php-fpm, on macOS and Linux, where
/// [`listener_holders`] and [`adopters`] name the workers of a master that
/// died from SIGKILL.
pub const FPM_ORPHAN_RECOVERY: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// Whether the daemon looks for a database server that a crashed `marsad`
/// left running, before it starts one: macOS and Linux (ADR 0023, section
/// 8), where the same orphan was measured, as for Caddy
/// ([`CADDY_ORPHAN_RECOVERY`]). On Windows the Job Object ends the servers
/// with the daemon (ADR 0017).
pub const DATABASE_ORPHAN_RECOVERY: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// How long `kill`, `ps`, or `lsof` may run.
#[cfg(unix)]
const TOOL_LIMIT: Duration = Duration::from_secs(2);

/// The signals [`send_signal`] sends. Defined in `services`, which has no
/// `full` feature, because a `ServiceSpec`'s stop method names them.
pub use crate::services::Signal;

/// Runs a tool with no input, for at most 2 s. `None` if it cannot start or
/// takes too long; `kill_on_drop` then stops it.
#[cfg(target_os = "macos")]
async fn run_tool(mut command: Command) -> Option<std::process::Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(TOOL_LIMIT, command.output())
        .await
        .ok()?
        .ok()
}

/// On Windows, sets `CREATE_NO_WINDOW`, so no console window opens for a child
/// of `marsad` (architecture.md section 9). Does nothing on macOS and Linux.
/// Not for the shim: its children must print to the user's terminal.
pub fn hide_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // Nothing to set on other systems; this only marks `cmd` as used.
    #[cfg(not(windows))]
    let _ = cmd;
}

// Helpers of the tests in the submodules.

#[cfg(test)]
fn at(text: &str) -> crate::ps::StartTime {
    text.parse().expect("a start time")
}

/// Asks `look` until `done` holds for its answer, for up to 10 s, and
/// gives that answer. With glibc's `posix_spawn` (Linux), `spawn` returns
/// before the child's exec is done, and until then its `exe` link names
/// this test binary.
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
async fn after_exec<T: std::fmt::Debug>(
    mut look: impl AsyncFnMut() -> T,
    done: impl Fn(&T) -> bool,
) -> T {
    let started = std::time::Instant::now();
    loop {
        let now = look().await;
        if done(&now) {
            return now;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{now:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
