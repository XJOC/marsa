//! The port check of setup on macOS (ADR 0008, "Herd conflict").
//! `/usr/sbin/netstat -anv -p tcp` lists the sockets that listen,
//! [`marsa_core::ports::holders`] picks those on 80 and 443, and `/bin/ps`
//! names each process. A holder, or a check that fails, means no
//! `pf_install`: the redirect would take that program's traffic without a
//! word. `marsa doctor` asks the same.

use marsa_core::platform::runner::MAX_OUTPUT;
use marsa_core::platform::runner::Runner;
use marsa_core::ports;
use marsa_core::ports::Listener;
use marsa_core::ports::ProcessInfo;
use marsa_core::types::PortHolder;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(super) const NETSTAT: &str = "/usr/sbin/netstat";

pub(super) const PS: &str = "/bin/ps";

/// The ports the `pf` redirect takes (ADR 0008).
const REDIRECTED: [u16; 2] = [80, 443];

/// How long `netstat` and each `ps` may take.
pub(in crate::daemon) const CHECK_LIMIT: Duration = Duration::from_secs(10);

/// At most this many `ps` calls; further holders keep netstat's name only.
pub(super) const MAX_PS: usize = 8;

/// Who listens on 80 and 443, or `None` if that cannot be known:
/// `netstat` failed, printed more than [`MAX_OUTPUT`] (the rest, with a
/// holder in it maybe, was not read), or listed no listening socket at
/// all. The daemon's own API socket always listens, so an empty list
/// means the output was not understood. The redirect never goes in blind.
/// `marsa doctor` asks the same (task M5-12).
pub fn port_holders(runner: &dyn Runner, stop: &CancellationToken) -> Option<Vec<PortHolder>> {
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
pub(super) fn holder(
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
pub(super) fn process(
    runner: &dyn Runner,
    pid: u32,
    stop: &CancellationToken,
) -> Option<ProcessInfo> {
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

#[cfg(test)]
mod tests {
    use super::super::osascript::OSASCRIPT;
    use marsa_core::platform::runner::MAX_OUTPUT;

    use super::super::test_support::*;
    use super::super::user_trust::SECURITY;
    use super::super::*;
    use super::*;
    use marsa_core::platform::runner::RunOutput;

    use marsa_core::types::PortHolder;

    use std::io;

    use std::time::Duration;

    #[test]
    fn herd_on_80_and_443_means_no_pf_install() {
        let dir = Dir::new(true);
        let answer = all_done(&["resolver_install"]);
        let fake = Fake::scripted(NETSTAT_HERD, move || ok(&answer));
        let report = dir.run(&fake);

        let nginx = |port| PortHolder {
            port,
            address: "127.0.0.1".into(),
            process: Some("nginx-arm64".into()),
            pid: Some(15464),
            user: Some("dev".into()),
            // The worker's title gives no hint; netstat's name says Herd.
            command: Some("nginx: worker process".into()),
            app: Some("Herd".into()),
        };
        assert_eq!(report.port_holders, [nginx(80), nginx(443)]);
        assert_eq!(report.skipped, [skip("pf_install", "redirect_port_held")]);
        assert_eq!(fake.request().ops, [resolver()]);
        assert_eq!(
            report.results,
            [done("resolver_install"), done("trust_install")]
        );
        assert_eq!(report.error, None);

        // `ps` once for the one PID that holds both ports.
        let calls = fake.calls();
        assert_eq!(
            fake.programs(),
            [NETSTAT, PS, OSASCRIPT, SECURITY, SECURITY]
        );
        assert_eq!(calls[0].args, ["-anv", "-p", "tcp"]);
        assert_eq!(calls[1].args, ["-p", "15464", "-o", "user=,comm="]);
        assert_eq!(calls[0].limit, Duration::from_secs(10));
        assert_eq!(calls[1].limit, Duration::from_secs(10));
    }

    #[test]
    fn netstat_output_cut_at_the_limit_means_no_pf_install() {
        // Real lines with nobody on 80 or 443, repeated to just under the
        // limit: read as usual.
        let free = netstat_free();
        let mut text = String::new();
        while text.len() + free.len() + 1 < MAX_OUTPUT as usize {
            text.push_str(free);
            text.push('\n');
        }
        let run = |text: String, done: &[&str]| {
            let dir = Dir::new(true);
            let answer = all_done(done);
            let fake = Fake::answering(move |program, args| match program {
                NETSTAT => ok(&text),
                OSASCRIPT => ok(&answer),
                SECURITY => security_untrusted(args),
                other => panic!("unexpected program {other}"),
            });
            let report = dir.run(&fake);
            (report, fake.request().ops, fake.programs())
        };
        let all = ["resolver_install", "pf_install"];
        let (report, ops, _) = run(text.clone(), &all);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(ops, [resolver(), pf(18080, 18443)]);
        assert_eq!(report.error, None);

        // Filled up to the limit, as `RealRunner` keeps a longer output: a
        // holder could sit in the part that was not read.
        text.push_str(&" ".repeat(MAX_OUTPUT as usize - text.len()));
        let (report, ops, programs) = run(text, &all[..1]);
        assert_eq!(report.skipped, [skip("pf_install", "ports_not_checked")]);
        assert_eq!(ops, [resolver()]);
        assert_eq!(programs, [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        assert_eq!(report.error, None);
    }

    #[test]
    fn netstat_that_fails_or_is_not_understood_means_no_pf_install() {
        let outputs: [fn() -> io::Result<RunOutput>; 3] = [
            || Err(io::Error::from(io::ErrorKind::NotFound)),
            || script_error("netstat: sysctl: Operation not permitted"),
            || ok("Active Internet connections\nsomething new\n"),
        ];
        for netstat in outputs {
            let dir = Dir::new(true);
            let answer = all_done(&["resolver_install"]);
            let fake = Fake::answering(move |program, args| match program {
                NETSTAT => netstat(),
                OSASCRIPT => ok(&answer),
                SECURITY => security_untrusted(args),
                other => panic!("unexpected program {other}"),
            });
            let report = dir.run(&fake);
            assert_eq!(report.skipped, [skip("pf_install", "ports_not_checked")]);
            assert_eq!(fake.request().ops, [resolver()]);
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
            assert_eq!(report.error, None);
        }
    }
}
