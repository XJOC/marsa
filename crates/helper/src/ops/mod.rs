//! Runs the ops of a valid request, in order, through [`System`].
//!
//! Every op gets its own result, and a failed op does not stop the next one.
//! An op meant for another OS fails with `op_not_supported`.
//!
//! Each pair of macOS ops has its own file, so their tasks do not touch the
//! same code: `resolver` (M4-07), `pf` (M4-08), and `trust` (M4-09). Linux
//! has `resolved` (M4-18a, M4-18h), `ports` (M4-18b), and `trust_linux` (M4-18c),
//! which uses the PEM parser of `trust`. Windows has `hosts` (M4-19d).

mod hosts;
mod marked;
pub mod pf;
mod ports;
mod program;
mod resolved;
mod resolver;
mod trust;
mod trust_linux;

use std::path::Path;

use crate::os::Os;
use crate::request::{Op, ValidRequest};
use crate::response::{OpResult, code};
use crate::system::System;

/// One result per op, and one per expanded op for `uninstall_all`. It takes
/// a [`ValidRequest`], so it cannot run a request that skipped validation.
pub fn apply(request: &ValidRequest, os: Os, system: &impl System) -> Vec<OpResult> {
    let mut results = Vec::with_capacity(request.ops().len());
    for op in request.ops() {
        run(op, os, system, &mut results);
    }
    results
}

/// The result of an op whose handler is not written yet.
pub fn not_implemented(op: &'static str) -> OpResult {
    let message = format!("{op} is not implemented yet");
    OpResult::failed(op, code::OP_NOT_IMPLEMENTED, message)
}

/// The folder of Marsa's root-owned files on macOS: the `pf` rules and
/// token, and the CA copy of the admin trust (ADR 0011). `pf_install` and
/// `trust_install` make it through `write_atomic`.
const MACOS_DIR: &str = "/Library/Application Support/Marsa";

fn run(op: &Op, os: Os, system: &impl System, results: &mut Vec<OpResult>) {
    if let Op::UninstallAll {} = op {
        for remove in removals(os) {
            run(&remove, os, system, results);
        }
        if os == Os::Macos {
            remove_folder_if_empty(system);
        }
    } else if supported(op, os) {
        results.push(handle(op, os, system));
    } else {
        let name = op.name();
        let message = format!("{name} is not supported on {}", os.name());
        results.push(OpResult::failed(name, code::OP_NOT_SUPPORTED, message));
    }
}

/// What `uninstall_all` runs: the reverse of the setup order, so the ports
/// (the `pf` redirect, the sysctl) go before the DNS setup, and the trust
/// goes last.
fn removals(os: Os) -> Vec<Op> {
    match os {
        Os::Macos => vec![Op::PfRemove {}, Op::ResolverRemove {}, Op::TrustRemove {}],
        Os::Linux => vec![
            Op::PortsRemove {},
            Op::ResolvedRemove {},
            Op::TrustRemove {},
        ],
        Os::Windows => vec![Op::HostsRemove {}],
    }
}

/// After the removals of `uninstall_all` on macOS: [`MACOS_DIR`] goes if
/// they left it empty, like `rmdir`, and never with what is inside. A folder
/// that still holds something (a removal failed and kept its file, or
/// another file is there), or that is gone already, stays as it is. That is
/// not a failure, and it adds no result: the results stay one per removal,
/// and a later `uninstall_all` tries again.
fn remove_folder_if_empty(system: &impl System) {
    // Every error is fine here, see above; stderr must stay empty (`main`).
    let _ = system.remove_empty_dir(Path::new(MACOS_DIR));
}

/// Windows has no `trust_*` for now: its helper service (task M4-19) accepts
/// only the hosts ops, and trust on Windows is decided there.
fn supported(op: &Op, os: Os) -> bool {
    match op {
        Op::UninstallAll {} => true,
        Op::TrustInstall { .. } | Op::TrustRemove {} => os != Os::Windows,
        Op::ResolverInstall { .. }
        | Op::ResolverRemove {}
        | Op::PfInstall { .. }
        | Op::PfRemove {} => os == Os::Macos,
        Op::ResolvedInstall { .. }
        | Op::ResolvedRemove {}
        | Op::PortsInstall {}
        | Op::PortsRemove {} => os == Os::Linux,
        Op::HostsSync { .. } | Op::HostsRemove {} => os == Os::Windows,
    }
}

/// One op meant for this OS. Every handler gets `system` for each file and
/// command, and returns the result of its op. Trust is written for macOS
/// first (task M4-09); Linux gets its own handler with task M4-18.
fn handle(op: &Op, os: Os, system: &impl System) -> OpResult {
    match op {
        Op::ResolverInstall { port } => resolver::install(*port, system),
        Op::ResolverRemove {} => resolver::remove(system),
        Op::PfInstall {
            http_port,
            https_port,
        } => pf::install(*http_port, *https_port, system),
        Op::PfRemove {} => pf::remove(system),
        Op::PortsInstall {} => ports::install(system),
        Op::PortsRemove {} => ports::remove(system),
        Op::ResolvedInstall { port } => resolved::install(*port, system),
        Op::ResolvedRemove {} => resolved::remove(system),
        Op::TrustInstall { pem } if os == Os::Macos => trust::install(pem, system),
        Op::TrustRemove {} if os == Os::Macos => trust::remove(system),
        // Linux: `supported` keeps the trust ops away from Windows.
        Op::TrustInstall { pem } => trust_linux::install(pem, system),
        Op::TrustRemove {} => trust_linux::remove(system),
        Op::HostsSync { names } => hosts::sync(names, system),
        Op::HostsRemove {} => hosts::remove(system),
        // `run` expands `uninstall_all` into its removals, so it never gets
        // here.
        Op::UninstallAll {} => not_implemented(op.name()),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;
    use crate::request::Request;
    use crate::response::Outcome;
    use crate::system::{FakeSystem, RunOutput};

    /// The op name and the failure code of each result, or `done` or
    /// `unchanged`. The request goes through `from_arg`, like a real one.
    fn codes(ops: Value, os: Os) -> Vec<(&'static str, &'static str)> {
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let fake = FakeSystem::default();
        let results = apply(&request, os, &fake);
        let code = |r: &OpResult| match r.outcome {
            Outcome::Failed { code, .. } => code,
            Outcome::Done => "done",
            Outcome::Unchanged => "unchanged",
        };
        results.iter().map(|r| (r.op, code(r))).collect()
    }

    fn every_op() -> Value {
        json!([
            { "op": "trust_install", "pem": "-----BEGIN CERTIFICATE-----\n" },
            { "op": "trust_remove" },
            { "op": "resolver_install", "port": 18053 },
            { "op": "resolver_remove" },
            { "op": "pf_install", "http_port": 18080, "https_port": 18443 },
            { "op": "pf_remove" },
            { "op": "resolved_install", "port": 18053 },
            { "op": "resolved_remove" },
            { "op": "ports_install" },
            { "op": "ports_remove" },
            { "op": "hosts_sync", "names": [] },
            { "op": "hosts_remove" },
        ])
    }

    #[test]
    fn an_op_for_another_os_is_not_supported_and_the_next_op_runs() {
        let ops = json!([
            { "op": "pf_install", "http_port": 18080, "https_port": 18443 },
            { "op": "trust_remove" },
        ]);
        // The fake has no trust tool, so nothing of Marsa's is trusted.
        let expected = [
            ("pf_install", code::OP_NOT_SUPPORTED),
            ("trust_remove", "unchanged"),
        ];
        assert_eq!(codes(ops, Os::Linux), expected);
    }

    #[test]
    fn handle_sends_each_op_to_its_own_handler() {
        // The fake has no `systemctl` and no trust tool, so on Linux each op
        // gets as far as its own first check.
        let ops = json!([
            { "op": "resolver_install", "port": 18053 },
            { "op": "resolved_install", "port": 18053 },
            { "op": "trust_remove" },
        ]);
        let linux = [
            ("resolver_install", code::OP_NOT_SUPPORTED),
            ("resolved_install", resolved::RESOLVED_NOT_RUNNING),
            ("trust_remove", "unchanged"),
        ];
        assert_eq!(codes(ops.clone(), Os::Linux), linux);
        let macos = codes(ops, Os::Macos);
        assert_eq!(macos[0], ("resolver_install", "done"));
        assert_eq!(macos[1], ("resolved_install", code::OP_NOT_SUPPORTED));
    }

    /// Only `op_not_supported` is checked: the other results change as the
    /// handlers get written.
    #[test]
    fn each_os_supports_only_its_own_ops() {
        let table = [
            (Os::Macos, &["trust", "resolver", "pf"][..]),
            (Os::Linux, &["trust", "resolved", "ports"]),
            (Os::Windows, &["hosts"]),
        ];
        for (os, own) in table {
            for (name, code) in codes(every_op(), os) {
                let prefix = name.rsplit_once('_').unwrap().0;
                let is_supported = code != code::OP_NOT_SUPPORTED;
                assert_eq!(is_supported, own.contains(&prefix), "{name} on {os:?}");
            }
        }
    }

    /// The files `pf_install` and `trust_install` leave in [`MACOS_DIR`],
    /// and a system on which every command succeeds.
    fn installed() -> FakeSystem {
        const TEST_CA: &str = include_str!("../../../../fixtures/helper/test-ca.pem");
        let mut fake = FakeSystem::default();
        let dir = Path::new(MACOS_DIR);
        let files = [
            ("pf-redirect.rules", "rdr pass on lo0\n"),
            ("pf-token", "7393882174112\n"),
            ("ca-root.pem", TEST_CA),
        ];
        for (name, text) in files {
            fake.write_atomic(&dir.join(name), text.as_bytes(), 0o644)
                .unwrap();
        }
        let exit_0 = RunOutput {
            code: Some(0),
            ..RunOutput::default()
        };
        for program in ["/sbin/pfctl", "/bin/launchctl", "/usr/bin/security"] {
            fake.defaults.insert(program, exit_0.clone());
        }
        fake
    }

    /// `uninstall_all` on `os`, with its op names and results.
    fn uninstall_all(fake: &FakeSystem, os: Os) -> Vec<(&'static str, &'static str)> {
        let raw = json!({ "version": 1, "ops": [{ "op": "uninstall_all" }] }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let code = |r: &OpResult| match r.outcome {
            Outcome::Failed { code, .. } => code,
            Outcome::Done => "done",
            Outcome::Unchanged => "unchanged",
        };
        let results = apply(&request, os, fake);
        results.iter().map(|r| (r.op, code(r))).collect()
    }

    #[test]
    fn uninstall_all_on_macos_removes_the_folder_it_emptied() {
        let fake = installed();
        assert!(fake.dirs.borrow().contains(Path::new(MACOS_DIR)));
        let results = uninstall_all(&fake, Os::Macos);
        assert_eq!(
            results,
            [
                ("pf_remove", "done"),
                ("resolver_remove", "unchanged"),
                ("trust_remove", "done"),
            ]
        );
        assert!(fake.files.borrow().is_empty(), "{:?}", fake.files.borrow());
        assert!(!fake.dirs.borrow().contains(Path::new(MACOS_DIR)));

        // Again: the folder is gone, which is fine, and adds no result.
        let again = uninstall_all(&fake, Os::Macos);
        let names: Vec<_> = again.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["pf_remove", "resolver_remove", "trust_remove"]);
        assert!(
            again.iter().all(|(_, code)| *code == "unchanged"),
            "{again:?}"
        );
    }

    #[test]
    fn a_folder_that_still_holds_a_file_stays_with_the_file() {
        // A removal that failed keeps its file, and the folder with it.
        let mut fake = installed();
        let rules = Path::new(MACOS_DIR).join("pf-redirect.rules");
        fake.fail_removes.insert(rules.clone());
        let results = uninstall_all(&fake, Os::Macos);
        assert_eq!(results[0], ("pf_remove", "pf_delete"));
        assert_eq!(results.len(), 3, "{results:?}");
        assert!(fake.exists(&rules));
        assert!(fake.dirs.borrow().contains(Path::new(MACOS_DIR)));

        // A file that is not Marsa's stays too: nothing is deleted by folder.
        let fake = installed();
        let other = Path::new(MACOS_DIR).join("notes.txt");
        fake.write_atomic(&other, b"mine", 0o644).unwrap();
        let results = uninstall_all(&fake, Os::Macos);
        assert_eq!(results.len(), 3, "{results:?}");
        assert_eq!(fake.read(&other, 16).unwrap(), b"mine");
        assert!(fake.dirs.borrow().contains(Path::new(MACOS_DIR)));
    }

    #[test]
    fn only_macos_touches_the_macos_folder() {
        for os in [Os::Linux, Os::Windows] {
            let fake = FakeSystem::default();
            fake.dirs.borrow_mut().insert(PathBuf::from(MACOS_DIR));
            uninstall_all(&fake, os);
            assert!(fake.dirs.borrow().contains(Path::new(MACOS_DIR)), "{os:?}");
        }
        // Nor does a single removal on macOS: only `uninstall_all` tidies.
        let fake = FakeSystem::default();
        fake.dirs.borrow_mut().insert(PathBuf::from(MACOS_DIR));
        let raw = json!({ "version": 1, "ops": [{ "op": "pf_remove" }] }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        apply(&request, Os::Macos, &fake);
        assert!(fake.dirs.borrow().contains(Path::new(MACOS_DIR)));
    }

    #[test]
    fn uninstall_all_expands_to_the_removals_of_this_os() {
        let table = [
            (
                Os::Macos,
                &["pf_remove", "resolver_remove", "trust_remove"][..],
            ),
            (
                Os::Linux,
                &["ports_remove", "resolved_remove", "trust_remove"],
            ),
            (Os::Windows, &["hosts_remove"]),
        ];
        for (os, expected) in table {
            let results = codes(json!([{ "op": "uninstall_all" }]), os);
            let names: Vec<_> = results.iter().map(|(name, _)| *name).collect();
            assert_eq!(names, expected, "{os:?}");
            assert!(results.iter().all(|(_, c)| *c != code::OP_NOT_SUPPORTED));
        }
    }
}
