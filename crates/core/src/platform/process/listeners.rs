//! Who listens on a port or holds a listener, and who may adopt the children
//! of a dead daemon.

use crate::caddy::Dial;
use crate::ports::PortLookup;

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "macos")]
use std::process::Stdio;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use tokio::process::Command;

#[cfg(target_os = "macos")]
use super::run_tool;

/// How long [`port_lookup`] waits for `lsof`. Longer than [`super::TOOL_LIMIT`]:
/// its answer decides a start, and on a busy Mac at login `lsof` can be
/// slow; a time-out would refuse the start for nothing.
#[cfg(target_os = "macos")]
const LOOKUP_LIMIT: Duration = Duration::from_secs(8);

/// The processes that hold a listener at `listen` open now: a Unix socket
/// bound to that path, or a TCP listener on `127.0.0.1:<port>`. A child that
/// inherited the listener from its parent holds it too. It says who holds a
/// listener there, not who inherited it from whom: a filter for the workers
/// of a php-fpm (task M4-25), never a proof alone. The caller checks each
/// pid with [`super::real_identity`] too. macOS asks `/usr/sbin/lsof` once, and in
/// the same run for the files of this process: if `lsof` does not describe
/// this process, or a signal ended it, its answer proves nothing. `None`
/// when it cannot be told: then nothing is signalled.
#[cfg(target_os = "macos")]
pub async fn listener_holders(listen: &Dial) -> Option<Vec<u32>> {
    let own = std::process::id();
    let mut lsof = Command::new("/usr/sbin/lsof");
    // No `-a`: the files of this process, or the sockets asked for.
    lsof.args(["-nP", "-p", &own.to_string()]);
    match listen {
        Dial::Unix(_) => lsof.arg("-U"),
        Dial::Tcp(port) => {
            let address = format!("-iTCP@127.0.0.1:{port}");
            lsof.args([address.as_str(), "-sTCP:LISTEN"])
        }
    };
    lsof.arg("-FptPnT").env("LC_ALL", "C");
    let out = run_tool(lsof).await?;
    // 1: a selection matched nothing. No code at all: a signal ended `lsof`,
    // so its list may be cut short.
    if !matches!(out.status.code(), Some(0 | 1)) {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    crate::orphan::parse_lsof_holders(&text, listen, own)
}

/// The processes that hold the listener at `listen` open now, from `/proc`
/// with `std` only, with the meaning of the macOS version: a
/// filter, never a proof. The inodes of the listening sockets come from
/// `/proc/net/unix` or `/proc/net/tcp`, then each process's `fd` folder is
/// read for them. A process whose files cannot be read (another user's, or
/// gone) does not hold it here, so it is never signalled for it. `None` when
/// the socket tables or `/proc` cannot be read.
#[cfg(target_os = "linux")]
pub async fn listener_holders(listen: &Dial) -> Option<Vec<u32>> {
    let inodes = match listen {
        Dial::Unix(path) => {
            use std::os::unix::ffi::OsStrExt as _;
            // Bytes: another user's socket path may not be UTF-8.
            let table = fs::read("/proc/net/unix").ok()?;
            crate::orphan::parse_proc_net_unix(&table, path.as_os_str().as_bytes())?
        }
        Dial::Tcp(port) => {
            let table = fs::read_to_string("/proc/net/tcp").ok()?;
            crate::orphan::parse_proc_net_tcp(&table, *port)
        }
    };
    let mut holders = Vec::new();
    if inodes.is_empty() {
        return Some(holders);
    }
    // If not even this process's files can be read, nobody's can: the list
    // would say "nobody" without having looked.
    fs::read_dir("/proc/self/fd").ok()?;
    for entry in fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        let holds = fds.flatten().any(|fd| {
            fs::read_link(fd.path())
                .ok()
                .and_then(|target| crate::orphan::socket_inode(&target))
                .is_some_and(|inode| inodes.contains(&inode))
        });
        if holds {
            holders.push(pid);
        }
    }
    holders.sort_unstable();
    Some(holders)
}

/// Not known on this system: nothing is signalled.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn listener_holders(_listen: &Dial) -> Option<Vec<u32>> {
    None
}

/// The processes that may have adopted a child of an earlier `marsad`
/// after its death: on macOS pid 1 (launchd) only. On Linux an orphan goes
/// to its nearest subreaper, such as `systemd --user`, not always to pid 1
/// (ADR 0025), and which one cannot be read without `prctl`. So: every
/// ancestor of this daemon, up to pid 1, which is always there unless the
/// daemon is pid 1 itself (a container: then it has no orphans). The daemon
/// starts detached, so the daemon that crashed may have had others: its
/// orphans under a subreaper that is not one of these are not found (a known
/// limit of `php::orphan`). Never this daemon itself. A list to search,
/// never a proof: each child found there is checked like any other.
#[cfg(target_os = "macos")]
pub async fn adopters() -> Vec<u32> {
    vec![1]
}

/// See the macOS version.
#[cfg(target_os = "linux")]
pub async fn adopters() -> Vec<u32> {
    let mut adopters = Vec::new();
    let mut pid = std::process::id();
    // A chain longer than this is no process tree; stop at pid 1 at the latest.
    for _ in 0..64 {
        let stat = fs::read_to_string(Path::new("/proc").join(pid.to_string()).join("stat"));
        let Some(stat) = stat
            .ok()
            .and_then(|text| crate::orphan::parse_proc_stat(&text))
        else {
            break;
        };
        // pid 0: the parent of pid 1, or of a process whose parent is in
        // another pid namespace.
        if stat.ppid == 0 || adopters.contains(&stat.ppid) {
            break;
        }
        adopters.push(stat.ppid);
        pid = stat.ppid;
    }
    if !adopters.contains(&1) {
        adopters.push(1);
    }
    // In a container the daemon may be pid 1 itself.
    let own = std::process::id();
    adopters.retain(|pid| *pid != own);
    adopters
}

/// Nothing adopts here: no orphan of the daemon outlives it (Windows: a Job
/// Object, ADR 0017).
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn adopters() -> Vec<u32> {
    Vec::new()
}

/// Who listens on TCP `port`, as `name(pid)`, for messages only: never use it
/// to pick a pid to signal. `None` when [`port_lookup`] finds nobody, fails,
/// or does not look on this system.
pub async fn port_owner(port: u16) -> Option<String> {
    match port_lookup(port).await {
        PortLookup::Owner(owner) => Some(owner),
        PortLookup::Nobody | PortLookup::Failed | PortLookup::NotChecked => None,
    }
}

/// Who listens on TCP `port`, on any address: macOS asks `/usr/sbin/lsof`,
/// which sees a listener on `*:<port>` that a bind to `127.0.0.1` misses
/// (std's bind sets `SO_REUSEADDR`). `lsof` exits with 1 and prints nothing
/// at all when nothing listens; any other answer, an error message, or no
/// answer within 8 s is [`PortLookup::Failed`], so a caller can fail closed.
/// `-w`: no warnings about other file systems on stderr.
#[cfg(target_os = "macos")]
pub async fn port_lookup(port: u16) -> PortLookup {
    let port = format!("-iTCP:{port}");
    lsof_lookup(&[&port, "-sTCP:LISTEN"]).await
}

/// Who has UDP `port` open, as `name(pid)`, for messages only (task
/// M4-18i: the DNS port). `None` when nobody has it, the look fails, or this
/// system has no look: only macOS asks `lsof`.
pub async fn udp_port_owner(port: u16) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let port = format!("-iUDP:{port}");
        match lsof_lookup(&[&port]).await {
            PortLookup::Owner(owner) => Some(owner),
            PortLookup::Nobody | PortLookup::Failed | PortLookup::NotChecked => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = port;
        None
    }
}

/// Runs `/usr/sbin/lsof` with `select` (the sockets to look at) and reads
/// the first process, as [`port_lookup`] says.
#[cfg(target_os = "macos")]
async fn lsof_lookup(select: &[&str]) -> PortLookup {
    let mut lsof = Command::new("/usr/sbin/lsof");
    // `+c 0`: the whole command name, not its first 9 characters.
    lsof.args(["-w", "+c", "0", "-nP"])
        .args(select)
        .arg("-Fpc")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let Ok(Ok(out)) = tokio::time::timeout(LOOKUP_LIMIT, lsof.output()).await else {
        return PortLookup::Failed;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    match (out.status.code(), crate::orphan::parse_lsof(&text)) {
        (Some(0), Some(owner)) => PortLookup::Owner(owner),
        (Some(1), None) if out.stdout.is_empty() && out.stderr.is_empty() => PortLookup::Nobody,
        _ => PortLookup::Failed,
    }
}

/// No look on this system: Linux often has no `lsof`, and there a bind to
/// `127.0.0.1` already fails next to a listener on every address.
#[cfg(not(target_os = "macos"))]
pub async fn port_lookup(_port: u16) -> PortLookup {
    PortLookup::NotChecked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// lsof runs on macOS only. The listener is this test's own.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn port_lookup_names_a_listener_and_tells_nobody_apart() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let PortLookup::Owner(owner) = port_lookup(port).await else {
            panic!("no owner for {port}");
        };
        assert!(
            owner.ends_with(&format!("({})", std::process::id())),
            "{owner}"
        );
        drop(listener);
        assert_eq!(port_lookup(port).await, PortLookup::Nobody);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn udp_port_owner_names_the_socket_and_then_nobody() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        let owner = udp_port_owner(port)
            .await
            .expect("this test holds the port");
        assert!(
            owner.ends_with(&format!("({})", std::process::id())),
            "{owner}"
        );
        drop(socket);
        assert_eq!(udp_port_owner(port).await, None);
    }

    /// Pid 1 always, and on Linux this process's parent first.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn adopters_end_at_pid_1() {
        let adopters = adopters().await;
        if std::process::id() == 1 {
            return;
        }
        assert_eq!(adopters.last(), Some(&1), "{adopters:?}");
        #[cfg(target_os = "linux")]
        {
            let parent = std::os::unix::process::parent_id();
            assert_eq!(adopters.first(), Some(&parent), "{adopters:?}");
        }
        assert!(!adopters.contains(&std::process::id()));
    }

    // The systems that list the holders of a listener: `lsof` on macOS,
    // `/proc` on Linux.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn listener_holders_are_the_processes_with_the_socket_or_port_open() {
        let own = std::process::id();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("php-8.4.sock");
        let socket = Dial::Unix(path.clone());
        // Nobody holds it yet: an empty list, which is not `None`.
        assert_eq!(listener_holders(&socket).await, Some(Vec::new()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // A process that does not hold it is not named.
        let mut sleeper = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert_eq!(listener_holders(&socket).await, Some(vec![own]));
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
        drop(listener);
        // The file is still there, and nobody holds the socket.
        assert!(path.exists());
        assert_eq!(listener_holders(&socket).await, Some(Vec::new()));

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = Dial::Tcp(listener.local_addr().unwrap().port());
        assert_eq!(listener_holders(&port).await, Some(vec![own]));
        drop(listener);
        assert_eq!(listener_holders(&port).await, Some(Vec::new()));
    }
}
