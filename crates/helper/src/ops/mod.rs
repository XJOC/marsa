//! Runs the ops of a valid request, in order, through [`System`].
//!
//! Every op gets its own result, and a failed op does not stop the next one.
//! An op meant for another OS fails with `op_not_supported`.
//!
//! Each pair of macOS ops has its own file, so their tasks do not touch the
//! same code: `resolver` (M4-07), `pf` (M4-08), and `trust` (M4-09). On
//! Linux, `ports` (M4-18b) is written; on Windows, `hosts` (M4-19d) and
//! `trust_windows` (M4-19e). The other Linux ops fail with
//! `op_not_implemented` until their tasks.

mod hosts;
mod marked;
pub mod pf;
mod ports;
mod program;
mod resolver;
mod trust;
mod trust_windows;

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

/// The SHA-256 (upper-case hex) of the one certificate in `pem`, or `None`
/// if `pem` is not exactly one certificate. The Windows service compares it
/// with the hash approved at install, before any op runs.
// Only the Windows service calls it; its tests run on every system.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn certificate_sha256(pem: &str) -> Option<String> {
    trust::parse_pem(pem)
        .ok()
        .map(|der| trust::sha256_hex(&der))
}

/// The result of an op whose handler is not written yet.
pub fn not_implemented(op: &'static str) -> OpResult {
    let message = format!("{op} is not implemented yet");
    OpResult::failed(op, code::OP_NOT_IMPLEMENTED, message)
}

fn run(op: &Op, os: Os, system: &impl System, results: &mut Vec<OpResult>) {
    if let Op::UninstallAll {} = op {
        for remove in removals(os) {
            run(&remove, os, system, results);
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
        Os::Windows => vec![Op::HostsRemove {}, Op::TrustRemove {}],
    }
}

/// Which ops each OS has. Trust is on every OS, each with its own tool.
fn supported(op: &Op, os: Os) -> bool {
    match op {
        Op::UninstallAll {} | Op::TrustInstall { .. } | Op::TrustRemove {} => true,
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
        Op::TrustInstall { pem } if os == Os::Macos => trust::install(pem, system),
        Op::TrustRemove {} if os == Os::Macos => trust::remove(system),
        Op::TrustInstall { pem } if os == Os::Windows => trust_windows::install(pem, system),
        Op::TrustRemove {} if os == Os::Windows => trust_windows::remove(system),
        Op::TrustInstall { .. } | Op::TrustRemove {} => not_implemented(op.name()),
        Op::HostsSync { names } => hosts::sync(names, system),
        Op::HostsRemove {} => hosts::remove(system),
        // `run` expands `uninstall_all` into its removals, so it never gets
        // here. Linux comes with its own tasks.
        Op::UninstallAll {} | Op::ResolvedInstall { .. } | Op::ResolvedRemove {} => {
            not_implemented(op.name())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use serde_json::{Value, json};

    use super::*;
    use crate::request::Request;
    use crate::response::Outcome;
    use crate::system::FakeSystem;

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
        let expected = [
            ("pf_install", code::OP_NOT_SUPPORTED),
            ("trust_remove", code::OP_NOT_IMPLEMENTED),
        ];
        assert_eq!(codes(ops, Os::Linux), expected);
    }

    #[test]
    fn handle_sends_each_op_to_its_own_handler() {
        // Ops that stay stubs here are the Linux ones, so this test does not
        // change when the macOS handlers land.
        let ops = json!([
            { "op": "resolver_install", "port": 18053 },
            { "op": "resolved_install", "port": 18053 },
            { "op": "trust_remove" },
        ]);
        let linux = [
            ("resolver_install", code::OP_NOT_SUPPORTED),
            ("resolved_install", code::OP_NOT_IMPLEMENTED),
            ("trust_remove", code::OP_NOT_IMPLEMENTED),
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
            (Os::Windows, &["hosts", "trust"]),
        ];
        for (os, own) in table {
            for (name, code) in codes(every_op(), os) {
                let prefix = name.rsplit_once('_').unwrap().0;
                let is_supported = code != code::OP_NOT_SUPPORTED;
                assert_eq!(is_supported, own.contains(&prefix), "{name} on {os:?}");
            }
        }
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
            (Os::Windows, &["hosts_remove", "trust_remove"]),
        ];
        for (os, expected) in table {
            let results = codes(json!([{ "op": "uninstall_all" }]), os);
            let names: Vec<_> = results.iter().map(|(name, _)| *name).collect();
            assert_eq!(names, expected, "{os:?}");
            assert!(results.iter().all(|(_, c)| *c != code::OP_NOT_SUPPORTED));
        }
    }
}
