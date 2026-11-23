//! Whether a pool's socket or port is free.

use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
// Only the Unix socket probe waits for a connection.
#[cfg(unix)]
use std::time::Duration;

use marsa_core::caddy::Dial;

/// How long a probe of the socket may wait for a connection.
// Only the Unix socket probe uses it, and Unix sockets exist on Unix only.
#[cfg(unix)]
const CONNECT_LIMIT: Duration = Duration::from_millis(500);

/// `the socket <path>` or `port 127.0.0.1:<port>`, for messages.
pub(super) fn describe(listen: &Dial) -> String {
    match listen {
        Dial::Unix(path) => format!("the socket {}", path.display()),
        Dial::Tcp(port) => format!("port 127.0.0.1:{port}"),
    }
}

/// Whether nothing listens at `listen` now.
pub async fn listen_free(listen: &Dial) -> bool {
    match listen {
        // The listener closes at once.
        Dial::Tcp(port) => TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).is_ok(),
        Dial::Unix(path) => socket_free(path).await,
    }
}

/// Free when a connection is refused or there is no socket file. A
/// connection, or one that does not complete in time, is busy. On Linux a
/// frozen listener with a full backlog makes the connection wait, so it is
/// busy; on macOS a full backlog refuses the connection, so it reads as
/// free. So this probe is never the proof that our php-fpm is gone: a master
/// is waited for by its pid ([`Orphans::wait_gone`]), and after a SIGKILL of
/// the master, or once its pid changed hands, its workers are searched for
/// whatever the probe says ([`Orphans::after_master`]). The probe alone
/// decides only whether that search runs otherwise, and whether the pid
/// file is deleted at the end.
// Unix sockets exist on Unix only, and only Unix runs php-fpm pools.
#[cfg(unix)]
async fn socket_free(path: &Path) -> bool {
    let connect = tokio::net::UnixStream::connect(path);
    match tokio::time::timeout(CONNECT_LIMIT, connect).await {
        Ok(Err(err)) => matches!(
            err.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
        ),
        Ok(Ok(_)) | Err(_) => false,
    }
}

/// No Unix sockets, and no php-fpm pools: nothing listens.
#[cfg(not(unix))]
async fn socket_free(_path: &Path) -> bool {
    true
}
