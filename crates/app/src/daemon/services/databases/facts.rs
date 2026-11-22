//! What the system says before a start: who holds the port, and who runs
//! at the pid of the server's own pid file (`postmaster.pid`), for a server
//! that has one.

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};

use marsa_core::databases::{PidFileCheck, StartFacts, classify_postmaster, port_check};
use marsa_core::platform::process;

use crate::daemon::services::probe::ProcessProbe;

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
    probes: &dyn ProcessProbe,
    port: u16,
    pid_file: &Path,
    pid_of: fn(&str) -> Option<u32>,
    servers: &[PathBuf],
) -> StartFacts {
    let facts = port_facts(probes, port).await;
    let text = process::read_small_file(pid_file);
    let pid_file = match text.as_deref().and_then(pid_of) {
        None => PidFileCheck::NoPid,
        Some(pid) => classify_postmaster(pid, &probes.real_identity(pid).await, servers),
    };
    StartFacts { pid_file, ..facts }
}

/// [`start_facts`] for a server with no pid file of its own (Valkey): the
/// port only, and [`PidFileCheck::NoPid`].
pub(super) async fn port_facts(probes: &dyn ProcessProbe, port: u16) -> StartFacts {
    // The listener closes at once.
    let bound = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
    let lookup = probes.port_lookup(port).await;
    StartFacts {
        port,
        port_check: port_check(bound, lookup),
        pid_file: PidFileCheck::NoPid,
    }
}
