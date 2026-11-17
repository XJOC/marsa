//! Linux: which way `.test` names go, as `marsad` reads it as a user, and
//! the helper operations of setup for it (ADR 0014, task M4-18k).
//!
//! | Way ([`dns::linux_path`]) | Setup sends |
//! | --- | --- |
//! | the `marsa0` link | `resolved_install`, then `hosts_remove` if the hosts file still has Marsa's block |
//! | the hosts file | `resolved_remove`, then `hosts_sync` with the name of every site |
//!
//! On the hosts way, `resolved_remove` takes out a link of an earlier
//! setup: it would meet the race of ADR 0014 (finding 30) at every boot.
//! With no link it changes nothing. On the link way, `hosts_remove` takes
//! out the block of an earlier setup, for example from before an upgrade to
//! systemd 250.
//!
//! The facts come from `systemctl --version`, `systemctl is-active
//! systemd-resolved.service`, and on systemd 249 only, the state of
//! systemd-networkd. All three run as the user. The hosts file is read
//! only, and every user may read it. Nothing here asks for the password.
//!
//! `marsa-helper` stays the judge (manager decision, task M4-18k2). If it
//! answers `resolved_use_hosts` or `resolved_not_running` to
//! `resolved_install` ([`helper_chose_hosts`]), setup logs the difference,
//! lists `resolved_install` as skipped with that code, and sends
//! `hosts_sync` in a second request. Only then does one `marsa setup` show
//! the password prompt twice: when the two copies of the rule disagree.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use marsa_core::dns::{self, LinuxFacts, LinuxPath, NETWORKD_UNITS};
use marsa_core::helper::{HelperOp, OpResult, OpStatus};
use tokio_util::sync::CancellationToken;

use super::{CHECK_LIMIT, Runner};

/// `systemctl`, at the path of Debian, Ubuntu, and Fedora. `marsad` never
/// searches `PATH`.
pub const SYSTEMCTL: &str = "/usr/bin/systemctl";
/// The hosts file. Never from a request.
pub const HOSTS_FILE: &str = "/etc/hosts";
/// The most of the hosts file that is read: the helper's own limit. A
/// larger file counts as one with no block, as the helper leaves it alone.
const MAX_HOSTS_BYTES: u64 = 16 * 1024 * 1024;
/// `systemctl is-active` for a unit that is not active (LSB "not running").
const INACTIVE: i32 = 3;

pub const RESOLVED_INSTALL: &str = "resolved_install";
/// The helper's answers to `resolved_install` that mean "use the hosts
/// file here".
const HOSTS_ANSWERS: [&str; 2] = ["resolved_use_hosts", "resolved_not_running"];

/// The facts of [`dns::linux_path`], each read with `systemctl`.
/// systemd-networkd is asked about only where the answer counts: on
/// systemd 249, and only while systemd-resolved may run.
pub fn facts(runner: &dyn Runner, stop: &CancellationToken) -> LinuxFacts {
    let systemd = output(runner, &["--version"], stop)
        .filter(|(code, _)| *code == 0)
        .and_then(|(_, stdout)| dns::systemd_version(&stdout));
    let resolved_runs = match output(runner, &["is-active", "--quiet", RESOLVED], stop) {
        Some((0, _)) => Some(true),
        Some((INACTIVE, _)) => Some(false),
        _ => None,
    };
    let asks_networkd = resolved_runs != Some(false)
        && systemd.is_some_and(|version| {
            (dns::LINK_BEFORE_250..dns::FIRST_LINK_SYSTEMD).contains(&version)
        });
    let networkd_may_run = asks_networkd && {
        let states = |verb: &str| {
            let args: Vec<&str> = [verb].into_iter().chain(NETWORKD_UNITS).collect();
            output(runner, &args, stop).map(|(_, stdout)| stdout)
        };
        dns::networkd_may_run(
            states("is-active").as_deref(),
            states("is-enabled").as_deref(),
        )
    };
    let facts = LinuxFacts {
        systemd,
        resolved_runs,
        networkd_may_run,
    };
    tracing::debug!(?facts, "the system's DNS, as the user sees it");
    facts
}

const RESOLVED: &str = "systemd-resolved.service";

/// The exit code and the output of `systemctl <args>`, or `None` if it did
/// not run to its end: it could not start, took longer than
/// [`CHECK_LIMIT`], or a signal stopped it.
fn output(runner: &dyn Runner, args: &[&str], stop: &CancellationToken) -> Option<(i32, String)> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    match runner.run(SYSTEMCTL, &args, CHECK_LIMIT, stop) {
        Ok(output) => {
            let code = output.code?;
            Some((code, String::from_utf8_lossy(&output.stdout).into_owned()))
        }
        Err(err) => {
            tracing::warn!(error = %err, "cannot run systemctl");
            None
        }
    }
}

/// The names in Marsa's block of the hosts file at `path`, or `None`: no
/// file, no block, markers that do not pair up, or a file that cannot be
/// read or is larger than the helper reads.
pub fn read_block(path: &Path) -> Option<BTreeSet<String>> {
    let read = || -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_HOSTS_BYTES + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    match read() {
        Ok(bytes) if bytes.len() as u64 > MAX_HOSTS_BYTES => {
            tracing::warn!(path = %path.display(), "the hosts file is larger than Marsa reads");
            None
        }
        Ok(bytes) => dns::hosts_block_names(&String::from_utf8_lossy(&bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "cannot read the hosts file");
            None
        }
    }
}

/// The DNS operations of setup for `path`. `block`: the hosts file has
/// Marsa's block now. `names`: the host names of the sites, for the hosts
/// way.
pub fn setup_ops(path: LinuxPath, block: bool, names: &[String], port: u16) -> Vec<HelperOp> {
    match path {
        LinuxPath::Link if block => vec![HelperOp::ResolvedInstall { port }, HelperOp::HostsRemove],
        LinuxPath::Link => vec![HelperOp::ResolvedInstall { port }],
        LinuxPath::Hosts => vec![
            HelperOp::ResolvedRemove,
            HelperOp::HostsSync {
                names: names.to_vec(),
            },
        ],
    }
}

/// The code of a failed `resolved_install` that means the helper chose the
/// hosts file: `resolved_use_hosts` (its systemd rule) or
/// `resolved_not_running`.
pub fn helper_chose_hosts(results: &[OpResult]) -> Option<String> {
    results
        .iter()
        .filter(|result| result.op == RESOLVED_INSTALL && result.status == OpStatus::Failed)
        .find_map(|result| {
            result
                .code
                .as_deref()
                .filter(|code| HOSTS_ANSWERS.contains(code))
                .map(str::to_owned)
        })
}
