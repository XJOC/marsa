//! What the system says before a start: who holds the port, and who runs
//! at the pid of the server's own pid file (`postmaster.pid`).

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};

use marsa_core::databases::{PidFileCheck, StartFacts, classify_postmaster, port_check};
use marsa_core::orphan::Liveness;
use marsa_core::platform::process::{self, Signal};
use marsa_core::ports::PortLookup;

use crate::daemon::caddy::orphan::BoxFuture;

/// Who runs under a pid, and who listens on a port: the OS in the daemon
/// ([`SystemProbes`]), a fake in tests. Signals only our old server, checked
/// right before ([`super::orphan::clear`]).
pub(super) trait Probes: fmt::Debug + Send + Sync {
    /// By the file the process really runs:
    /// [`marsa_core::platform::process::real_identity`].
    fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness>;
    /// Who listens on the port, on any address:
    /// [`marsa_core::platform::process::port_lookup`]. An owner, or a look
    /// that failed, means busy.
    fn port_lookup(&self, port: u16) -> BoxFuture<'_, PortLookup>;
    /// Sends `signal` to `pid`: only to our old server, checked right
    /// before ([`super::orphan::clear`]).
    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>>;
}

/// The real OS.
#[derive(Debug)]
pub(super) struct SystemProbes;

impl Probes for SystemProbes {
    fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::real_identity(pid))
    }

    fn port_lookup(&self, port: u16) -> BoxFuture<'_, PortLookup> {
        Box::pin(process::port_lookup(port))
    }

    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(process::send_signal(pid, signal))
    }
}

/// The facts `decide_start` needs, read from the system now:
///
/// - the port: a bind to `127.0.0.1:<port>`, and who listens on it on any
///   address; a look that fails refuses too ([`port_check`]);
/// - the server's own pid file `pid_file` (`postmaster.pid`): read with a
///   size cap, never through a link ([`process::read_small_file`]), its pid
///   by `pid_of`, and the program at that pid, by its real executable
///   ([`classify_postmaster`] against `servers`, from
///   [`super::init::installed_servers`]).
///
/// The caller first makes sure that our own supervisor does not run the
/// server: that one holds the port and the file, and is not busy.
pub(super) async fn start_facts(
    probes: &dyn Probes,
    port: u16,
    pid_file: &Path,
    pid_of: fn(&str) -> Option<u32>,
    servers: &[PathBuf],
) -> StartFacts {
    // The listener closes at once.
    let bound = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
    let lookup = probes.port_lookup(port).await;
    let text = process::read_small_file(pid_file);
    let pid_file = match text.as_deref().and_then(pid_of) {
        None => PidFileCheck::NoPid,
        Some(pid) => classify_postmaster(pid, &probes.real_identity(pid).await, servers),
    };
    StartFacts {
        port,
        port_check: port_check(bound, lookup),
        pid_file,
    }
}
