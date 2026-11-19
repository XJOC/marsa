//! Linux: which way `.test` names go, as `marsad` reads it as a user, and
//! the helper operations of setup for it (ADR 0014, task M4-18k).
//!
//! | Way ([`dns::linux_path`]) | Setup's request | A second request, only if needed |
//! | --- | --- | --- |
//! | the `marsa0` link | `resolved_install` | `hosts_remove`, once the link is made, if the hosts file still has Marsa's block |
//! | the hosts file | `resolved_remove`, then `hosts_sync` with the name of every site | – |
//!
//! On the hosts way, `resolved_remove` takes out a link of an earlier
//! setup: it would meet the race of ADR 0014 (finding 30) at every boot.
//! With no link it changes nothing. On the link way, `hosts_remove` takes
//! out the block of an earlier setup, for example from before an upgrade to
//! systemd 250. It waits for the link: the helper runs every op of a
//! request even when one fails, and the block must stay while it is the
//! only way the sites resolve.
//!
//! `hosts_sync` is left out, with a skipped step, when the site list is not
//! known (`hosts_sites_unknown`): an empty list would empty the block. Also
//! past the helper's limits (`hosts_too_many_names`): it would refuse the
//! whole request, the certificate and the ports with it.
//!
//! The facts come from `systemctl --version`, `systemctl is-active
//! systemd-resolved.service`, and on systemd 249 only, the state of
//! systemd-networkd. All three run as the user. The hosts file is read
//! only, and every user may read it. Nothing here asks for the password.
//!
//! `marsa-helper` stays the judge (manager decision, task M4-18k2). If it
//! answers `resolved_use_hosts` or `resolved_not_running` to
//! `resolved_install` ([`helper_chose_hosts`]), setup logs the difference,
//! lists `resolved_install` as skipped with that code, and sends the
//! operations of the hosts way in a second request. So one `marsa setup`
//! shows the password prompt twice only when the two copies of the rule
//! disagree, or once after the move from the hosts file to the link.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use marsa_core::dns::{self, LinuxFacts, NETWORKD_UNITS};
use marsa_core::helper::{HelperOp, OpResult, OpStatus};
use marsa_core::sites;
use marsa_core::types::HostsStatus;
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

pub const RESOLVED_INSTALL: &str = "resolved_install";
pub const HOSTS_SYNC: &str = "hosts_sync";
pub const HOSTS_REMOVE: &str = "hosts_remove";
/// Why `hosts_sync` was left out: no site list.
pub const HOSTS_SITES_UNKNOWN: &str = "hosts_sites_unknown";
/// Why `hosts_sync` was left out: more names than the helper takes.
pub const HOSTS_TOO_MANY_NAMES: &str = "hosts_too_many_names";
/// The helper's answers to `resolved_install` that mean "use the hosts
/// file here".
const HOSTS_ANSWERS: [&str; 2] = ["resolved_use_hosts", "resolved_not_running"];

/// The facts of [`dns::linux_path`], each read with `systemctl`.
/// systemd-networkd is asked about only where the answer counts: on
/// systemd 249, and only while systemd-resolved runs.
pub fn facts(runner: &dyn Runner, stop: &CancellationToken) -> LinuxFacts {
    let systemd = output(runner, &["--version"], stop)
        .filter(|(code, _)| *code == 0)
        .and_then(|(_, stdout)| dns::systemd_version(&stdout));
    let resolved_runs = resolved_runs(runner, stop);
    let asks_networkd = resolved_runs == Some(true)
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

/// Whether systemd-resolved runs, as the helper reads it: exit 0 runs; any
/// other exit, or a `systemctl` that cannot start, does not (the helper
/// answers `resolved_not_running` there). `None` only when the read did not
/// end: it took longer than [`CHECK_LIMIT`], the daemon stops, or a signal
/// stopped it. The helper is asked then, so one slow `systemctl` never
/// takes out a link that works (review of #130).
fn resolved_runs(runner: &dyn Runner, stop: &CancellationToken) -> Option<bool> {
    let args = ["is-active", "--quiet", RESOLVED].map(OsString::from);
    match runner.run(SYSTEMCTL, &args, CHECK_LIMIT, stop) {
        Ok(output) => output.code.map(|code| code == 0),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ) =>
        {
            tracing::warn!(error = %err, "systemctl is-active did not end");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "cannot run systemctl, so systemd-resolved counts as stopped");
            Some(false)
        }
    }
}

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

/// The file in the data folder that says this folder's setup took the
/// hosts way: `<data>/hosts-block` (review of #132). Setup writes it after
/// a `hosts_sync` that is done, and takes it out once the link is made.
/// Only its presence counts. A data folder whose setup never wrote the
/// block, such as a test's or a scratch one, never syncs the hosts file
/// from `marsa park` or `marsa link`, so it never asks for the password.
pub fn marker(data_dir: &Path) -> PathBuf {
    data_dir.join(HOSTS_MARKER)
}

const HOSTS_MARKER: &str = "hosts-block";
const MARKER_TEXT: &str = "marsa setup put .test names in /etc/hosts for this data folder\n";

/// Records the way setup took, from its `results`: the marker after a
/// `hosts_sync` that is done, none once the link is made. A failure is a
/// warning only: without the marker, `marsa park` and `marsa link` leave
/// the hosts file to the next `marsa setup`.
pub fn record_way(data_dir: &Path, results: &[OpResult]) {
    let synced = results.iter().any(|result| {
        result.op == HOSTS_SYNC && matches!(result.status, OpStatus::Done | OpStatus::Unchanged)
    });
    let path = marker(data_dir);
    let outcome = if synced {
        fs::write(&path, MARKER_TEXT)
    } else if link_made(results) {
        fs::remove_file(&path).or_else(|err| match err.kind() {
            io::ErrorKind::NotFound => Ok(()),
            _ => Err(err),
        })
    } else {
        return;
    };
    if let Err(err) = outcome {
        tracing::warn!(path = %path.display(), error = %err, "cannot record the way of .test names");
    }
}

/// What `GET /v1/hosts` answers on Linux. The hosts file is used where this
/// data folder's setup wrote Marsa's block ([`marker`]) and the block is
/// still there; then `missing` lists the `names` it lacks. No program runs.
pub fn status(data_dir: &Path, hosts_file: Option<&Path>, names: &[String]) -> HostsStatus {
    if !marker(data_dir).is_file() {
        return HostsStatus::default();
    }
    let Some(block) = hosts_file.and_then(read_block) else {
        return HostsStatus::default();
    };
    let missing = names
        .iter()
        .filter(|name| !block.contains(*name))
        .cloned()
        .collect();
    HostsStatus {
        used: true,
        missing,
    }
}

/// The names a sync from `marsa park` or `marsa link` sends: the sites'
/// `names` and every name the block in `hosts_file` has now, sorted. Such a
/// sync only adds: a site whose folder cannot be read for a moment, or a
/// name of another data folder, keeps its line. `marsa setup` sends the
/// exact list. A name in the block that the helper would refuse (one added
/// by hand, such as `api.shop.test` or `Shop.test`) is left out: it would
/// make the helper refuse the whole request, after the password (review of
/// #132).
pub fn names_to_add(hosts_file: Option<&Path>, names: &[String]) -> Vec<String> {
    let block = hosts_file.and_then(read_block).unwrap_or_default();
    let mut all: BTreeSet<String> = block
        .into_iter()
        .filter(|name| {
            name.strip_suffix(".test")
                .is_some_and(|label| sites::validate_name(label).is_ok())
        })
        .collect();
    all.extend(names.iter().cloned());
    all.into_iter().collect()
}

/// The operations of the hosts way: `resolved_remove`, then `hosts_sync`
/// with `names`, the host names of the sites (`None`: not known). The
/// second value is why `hosts_sync` was left out, if it was.
pub fn hosts_ops(names: Option<&[String]>) -> (Vec<HelperOp>, Option<&'static str>) {
    let mut ops = vec![HelperOp::ResolvedRemove];
    let skipped = match names {
        None => Some(HOSTS_SITES_UNKNOWN),
        Some(names) if !dns::hosts_names_fit(names) => Some(HOSTS_TOO_MANY_NAMES),
        Some(names) => {
            ops.push(HelperOp::HostsSync {
                names: names.to_vec(),
            });
            None
        }
    };
    if let Some(code) = skipped {
        tracing::warn!(code, "hosts_sync left out of the request");
    }
    (ops, skipped)
}

/// Whether the helper made the `marsa0` link, now or before.
pub fn link_made(results: &[OpResult]) -> bool {
    results.iter().any(|result| {
        result.op == RESOLVED_INSTALL
            && matches!(result.status, OpStatus::Done | OpStatus::Unchanged)
    })
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn result(op: &str, status: OpStatus) -> OpResult {
        OpResult {
            op: op.to_owned(),
            status,
            code: None,
            message: None,
        }
    }

    /// The marker follows the way setup took: written after a `hosts_sync`
    /// that is done or unchanged, gone once the link is made, and left as it
    /// is otherwise (review of #132).
    #[test]
    fn the_marker_follows_the_way_setup_took() {
        let dir = TempDir::new().unwrap();
        let marked = || marker(dir.path()).is_file();
        record_way(dir.path(), &[result("hosts_sync", OpStatus::Failed)]);
        assert!(!marked());
        record_way(dir.path(), &[result("hosts_sync", OpStatus::Unchanged)]);
        assert!(marked());
        record_way(dir.path(), &[result("resolved_install", OpStatus::Failed)]);
        assert!(marked());
        record_way(dir.path(), &[result("resolved_install", OpStatus::Done)]);
        assert!(!marked());
        // Gone already: no error.
        record_way(
            dir.path(),
            &[result("resolved_install", OpStatus::Unchanged)],
        );
        assert!(!marked());
    }

    /// Used only with the marker and the block both there.
    #[test]
    fn the_status_needs_the_marker_and_the_block() {
        let dir = TempDir::new().unwrap();
        let hosts = dir.path().join("hosts");
        fs::write(&hosts, "# Marsa start\n127.0.0.1 a.test\n# Marsa end\n").unwrap();
        let names = ["a.test".to_owned(), "b.test".to_owned()];
        assert_eq!(
            status(dir.path(), Some(&hosts), &names),
            HostsStatus::default()
        );
        fs::write(marker(dir.path()), "").unwrap();
        let expected = HostsStatus {
            used: true,
            missing: vec!["b.test".to_owned()],
        };
        assert_eq!(status(dir.path(), Some(&hosts), &names), expected);
        assert_eq!(status(dir.path(), None, &names), HostsStatus::default());
        fs::write(&hosts, "127.0.0.1 localhost\n").unwrap();
        assert_eq!(
            status(dir.path(), Some(&hosts), &names),
            HostsStatus::default()
        );
    }

    /// A sync adds the sites to the block's names, and leaves out a name
    /// in the block the helper would refuse (review of #132).
    #[test]
    fn a_sync_keeps_the_blocks_names_the_helper_takes() {
        let dir = TempDir::new().unwrap();
        let hosts = dir.path().join("hosts");
        let block = "# Marsa start\n127.0.0.1 old.test\n127.0.0.1 api.shop.test\n\
                     127.0.0.1 Shop.test\n127.0.0.1 -x.test\n127.0.0.1 x.example\n# Marsa end\n";
        fs::write(&hosts, block).unwrap();
        let names = ["new.test".to_owned()];
        assert_eq!(names_to_add(Some(&hosts), &names), ["new.test", "old.test"]);
        assert_eq!(names_to_add(None, &names), ["new.test"]);
    }

    /// The block's names; nothing for no file, a folder, or a file larger
    /// than the helper reads (review of #130).
    #[test]
    fn the_block_is_read_up_to_the_helpers_limit() {
        let dir = TempDir::new().unwrap();
        let hosts = dir.path().join("hosts");
        assert_eq!(read_block(&hosts), None);
        assert_eq!(read_block(dir.path()), None);

        let block = "# Marsa start\n127.0.0.1 shop.test\n# Marsa end\n";
        fs::write(&hosts, block).unwrap();
        let names = read_block(&hosts).unwrap();
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["shop.test"]);

        let filler = "#".repeat(usize::try_from(MAX_HOSTS_BYTES).unwrap() - block.len());
        fs::write(&hosts, format!("{block}{filler}")).unwrap();
        assert!(read_block(&hosts).is_some());
        fs::write(&hosts, format!("{block}{filler}#")).unwrap();
        assert_eq!(read_block(&hosts), None);
    }
}
