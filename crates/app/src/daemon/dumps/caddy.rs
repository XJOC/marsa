//! Caddy follows the dump server: while it listens, every PHP site sends its
//! `dump()` to it; when it does not (off, or the port is taken), the dumps
//! stay in the page.

use std::net::SocketAddr;
use std::sync::Arc;

use marsa_core::types::{DumpsStatus, ServiceId};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::daemon::state::Daemon;

/// The address Caddy sends dumps to: where the dump server listens, and
/// only while its status says so.
pub(super) fn target(daemon: &Daemon) -> Option<SocketAddr> {
    let dumps = daemon.dumps();
    (dumps.status() == DumpsStatus::Listening)
        .then(|| dumps.local_addr())
        .flatten()
}

/// Gives Caddy the address now, then again at each change of the dump
/// server's status, until the daemon stops. A running Caddy gets the new
/// config at once; one that does not run reads it when it starts.
pub fn follow(daemon: &Arc<Daemon>) -> JoinHandle<()> {
    let daemon = Arc::clone(daemon);
    let mut status = daemon.dumps().watch_status();
    tokio::spawn(async move {
        // The start: Caddy does not run yet, so nothing to send.
        daemon.caddy().set_dumps(target(&daemon));
        loop {
            tokio::select! {
                () = daemon.shutdown().cancelled() => return,
                changed = status.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
            let addr = target(&daemon);
            if !daemon.caddy().set_dumps(addr) || !daemon.proc_alive(ServiceId::Caddy) {
                continue;
            }
            info!(?addr, "Caddy gets the new place of the dump server");
            if let Err(err) = daemon.caddy().reapply(&daemon).await {
                warn!(code = err.code(), %err, "Caddy did not get the dump server's address");
            }
        }
    })
}
