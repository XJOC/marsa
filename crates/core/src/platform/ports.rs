//! Who listens on ports 80 and 443, asked of the system through a [`Runner`].
//! `marsa setup` and `marsa doctor` both ask here, so there is one list of
//! programs, one time limit, and one fake for the tests.
//!
//! - macOS (ADR 0008, "Herd conflict"): `/usr/sbin/netstat -anv -p tcp` lists
//!   the sockets that listen, [`ports::holders`] picks those on 80 and 443,
//!   and `/bin/ps` names each process.
//! - Windows: `netstat -ano` lists them and `tasklist` names each process.
//!
//! A holder, or a check that fails (`None`), means no `pf_install`: the
//! redirect would take that program's traffic without a word. The text
//! parsing is pure and lives in [`marsa_core::ports`](crate::ports).

use crate::platform::browser::windows_dir;
use crate::platform::runner::MAX_OUTPUT;
use crate::platform::runner::Runner;
use crate::ports;
use crate::ports::Listener;
use crate::ports::ProcessInfo;
use crate::recipes::Os;
use crate::types::PortHolder;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const NETSTAT: &str = "/usr/sbin/netstat";

pub const PS: &str = "/bin/ps";

/// The ports the `pf` redirect takes (ADR 0008).
const REDIRECTED: [u16; 2] = [80, 443];

/// How long `netstat` and each `ps` may take.
pub const CHECK_LIMIT: Duration = Duration::from_secs(10);

/// At most this many `ps` calls; further holders keep netstat's name only.
pub const MAX_PS: usize = 8;

/// Who listens on 80 and 443, or `None` if that cannot be known:
/// `netstat` failed, printed more than [`MAX_OUTPUT`] (the rest, with a
/// holder in it maybe, was not read), or listed no listening socket at
/// all. The daemon's own API socket always listens, so an empty list
/// means the output was not understood. The redirect never goes in blind.
fn macos_holders(runner: &dyn Runner, stop: &CancellationToken) -> Option<Vec<PortHolder>> {
    let args = ["-anv", "-p", "tcp"].map(OsString::from);
    let output = match runner.run(NETSTAT, &args, CHECK_LIMIT, stop) {
        Ok(output) if output.success() => output,
        Ok(output) => {
            tracing::warn!(exit = ?output.code, "netstat failed; ports 80 and 443 not checked");
            return None;
        }
        Err(err) => {
            tracing::warn!(error = %err, "cannot run netstat; ports 80 and 443 not checked");
            return None;
        }
    };
    // `>=`: an output of exactly the limit may have been cut too.
    if output.stdout.len() as u64 >= MAX_OUTPUT {
        tracing::warn!(
            bytes = output.stdout.len(),
            "netstat printed more than Marsa reads; ports 80 and 443 not checked"
        );
        return None;
    }
    let listeners = ports::parse_netstat(&String::from_utf8_lossy(&output.stdout));
    if listeners.is_empty() {
        tracing::warn!("netstat listed no listening socket; its output was not understood");
        return None;
    }
    let mut processes = BTreeMap::new();
    let holders = REDIRECTED
        .iter()
        .flat_map(|&port| ports::holders(&listeners, port))
        .map(|listener| holder(runner, listener, &mut processes, stop))
        .collect();
    Some(holders)
}

/// One holder, named by `ps` (once per PID, at most [`MAX_PS`] PIDs) and
/// by netstat. [`ports::known_app`] tries the `ps` command first, then
/// the netstat name: an nginx worker shows only its title in `ps`.
fn holder(
    runner: &dyn Runner,
    listener: &Listener,
    processes: &mut BTreeMap<u32, Option<ProcessInfo>>,
    stop: &CancellationToken,
) -> PortHolder {
    let info = listener.pid.and_then(|pid| {
        if !processes.contains_key(&pid) && processes.len() >= MAX_PS {
            return None;
        }
        let info = processes
            .entry(pid)
            .or_insert_with(|| process(runner, pid, stop));
        info.clone()
    });
    let app = info
        .as_ref()
        .and_then(|info| ports::known_app(&info.command))
        .or_else(|| listener.process.as_deref().and_then(ports::known_app));
    PortHolder {
        port: listener.port,
        address: listener.address.clone(),
        process: listener.process.clone(),
        pid: listener.pid,
        user: info.as_ref().map(|info| info.user.clone()),
        command: info.map(|info| info.command),
        app: app.map(str::to_owned),
    }
}

/// The user and program of `pid`, or `None` if `ps` cannot tell: the
/// process may have ended since netstat listed it.
fn process(runner: &dyn Runner, pid: u32, stop: &CancellationToken) -> Option<ProcessInfo> {
    let pid_text = pid.to_string();
    let args = ["-p", pid_text.as_str(), "-o", "user=,comm="].map(OsString::from);
    match runner.run(PS, &args, CHECK_LIMIT, stop) {
        Ok(output) if output.success() => ports::parse_ps(&String::from_utf8_lossy(&output.stdout)),
        Ok(_) => None,
        Err(err) => {
            tracing::debug!(pid, error = %err, "cannot run ps");
            None
        }
    }
}

/// Who listens on 80 and 443 on `os`, or `None` if that cannot be known (the
/// command failed, printed more than [`MAX_OUTPUT`], or listed no listening
/// socket at all: the daemon's own API socket always listens, so an empty
/// list means the output was not understood). Systems without a check
/// (Linux) give `None`.
pub fn holders(runner: &dyn Runner, os: Os, stop: &CancellationToken) -> Option<Vec<PortHolder>> {
    match os {
        Os::Mac => macos_holders(runner, stop),
        Os::Windows => windows_holders(runner, stop),
        Os::Linux | Os::Any => None,
    }
}

/// `netstat.exe` and `tasklist.exe` in `System32`, from `SystemRoot`. Built
/// once, so the paths stay `&'static str` as [`Runner`] needs.
fn windows_tools() -> &'static (String, String) {
    static TOOLS: OnceLock<(String, String)> = OnceLock::new();
    TOOLS.get_or_init(|| {
        let system = windows_dir(std::env::var_os("SystemRoot")).join("System32");
        let path = |name: &str| system.join(name).to_string_lossy().into_owned();
        (path("NETSTAT.EXE"), path("tasklist.exe"))
    })
}

/// Windows: who listens on 80 and 443, from `netstat -ano`, each named by
/// `tasklist`. `None` if netstat failed or listed no listener.
fn windows_holders(runner: &dyn Runner, stop: &CancellationToken) -> Option<Vec<PortHolder>> {
    let (netstat, tasklist) = windows_tools();
    let args = [OsString::from("-ano")];
    let output = match runner.run(netstat, &args, CHECK_LIMIT, stop) {
        Ok(output) if output.success() => output,
        Ok(output) => {
            tracing::warn!(exit = ?output.code, "netstat failed; ports 80 and 443 not checked");
            return None;
        }
        Err(err) => {
            tracing::warn!(error = %err, "cannot run netstat; ports 80 and 443 not checked");
            return None;
        }
    };
    if output.stdout.len() as u64 >= MAX_OUTPUT {
        tracing::warn!("netstat printed more than Marsa reads; ports 80 and 443 not checked");
        return None;
    }
    let listeners = ports::parse_netstat_windows(&String::from_utf8_lossy(&output.stdout));
    if listeners.is_empty() {
        tracing::warn!("netstat listed no listening socket; its output was not understood");
        return None;
    }
    let mut names: BTreeMap<u32, Option<String>> = BTreeMap::new();
    let mut name_of = |pid: u32| {
        names
            .entry(pid)
            .or_insert_with(|| {
                let filter = format!("PID eq {pid}");
                let args = ["/FI", filter.as_str(), "/FO", "CSV", "/NH"].map(OsString::from);
                match runner.run(tasklist, &args, CHECK_LIMIT, stop) {
                    Ok(out) if out.success() => {
                        ports::parse_tasklist(&String::from_utf8_lossy(&out.stdout))
                    }
                    _ => None,
                }
            })
            .clone()
    };
    let mut holders = REDIRECTED
        .iter()
        .flat_map(|&port| ports::holders(&listeners, port))
        .map(|listener| {
            let process = listener.pid.and_then(&mut name_of);
            let app = process
                .as_deref()
                .and_then(ports::known_app)
                .map(str::to_owned);
            PortHolder {
                port: listener.port,
                address: listener.address.clone(),
                process,
                pid: listener.pid,
                user: None,
                command: None,
                app,
            }
        })
        .collect::<Vec<_>>();
    // One line per program and port: System holds 80 on IPv4 and IPv6.
    holders.dedup_by_key(|holder| (holder.port, holder.pid));
    Some(holders)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::runner::RunOutput;
    use std::io;
    use std::sync::Mutex;

    /// Answers by program and records the calls: runs nothing.
    struct Fake {
        netstat: io::Result<String>,
        calls: Mutex<Vec<(&'static str, Vec<OsString>)>>,
    }

    impl Runner for Fake {
        fn run(
            &self,
            program: &'static str,
            args: &[OsString],
            _limit: Duration,
            _stop: &CancellationToken,
        ) -> io::Result<RunOutput> {
            self.calls.lock().unwrap().push((program, args.to_vec()));
            let text = if program.to_lowercase().contains("netstat") {
                match &self.netstat {
                    Ok(text) => text.clone(),
                    Err(err) => return Err(io::Error::from(err.kind())),
                }
            } else {
                "\"caddy.exe\",\"5244\",\"Console\",\"2\",\"1 K\"".to_owned()
            };
            Ok(RunOutput {
                code: Some(0),
                stdout: text.into_bytes(),
                stderr: Vec::new(),
            })
        }
    }

    fn fake(netstat: io::Result<String>) -> Fake {
        Fake {
            netstat,
            calls: Mutex::default(),
        }
    }

    const WINDOWS: &str = "  TCP    127.0.0.1:80           0.0.0.0:0              LISTENING       5244\r\n  TCP    [::1]:80               [::]:0                 LISTENING       5244\r\n  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1012\r\n";

    #[test]
    fn windows_names_each_holder_once_through_the_runner() {
        let runner = fake(Ok(WINDOWS.to_owned()));
        let holders = holders(&runner, Os::Windows, &CancellationToken::new()).unwrap();
        assert_eq!(holders.len(), 1, "{holders:?}");
        assert_eq!(holders[0].port, 80);
        assert_eq!(holders[0].process.as_deref(), Some("caddy.exe"));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "netstat once, tasklist once for the PID");
        assert_eq!(calls[0].1, [OsString::from("-ano")]);
    }

    #[test]
    fn windows_netstat_that_fails_or_lists_nothing_is_none() {
        for netstat in [
            Err(io::Error::from(io::ErrorKind::NotFound)),
            Ok("something new\r\n".to_owned()),
        ] {
            let runner = fake(netstat);
            assert_eq!(
                holders(&runner, Os::Windows, &CancellationToken::new()),
                None
            );
        }
    }

    #[test]
    fn linux_has_no_check() {
        let runner = fake(Ok(String::new()));
        assert_eq!(holders(&runner, Os::Linux, &CancellationToken::new()), None);
        assert!(runner.calls.lock().unwrap().is_empty());
    }
}
