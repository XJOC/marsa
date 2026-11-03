//! Runs the ops of a valid request, in order, through [`System`].
//!
//! Every op gets its own result, and a failed op does not stop the next one.
//! An op meant for another OS fails with `op_not_supported`. The handlers are
//! stubs that fail with `op_not_implemented` until their tasks (M4-07 to
//! M4-09).

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

fn run(op: &Op, os: Os, system: &impl System, results: &mut Vec<OpResult>) {
    if let Op::UninstallAll {} = op {
        for remove in removals(os) {
            run(&remove, os, system, results);
        }
    } else if supported(op, os) {
        results.push(handle(op, system));
    } else {
        let name = op.name();
        let message = format!("{name} is not supported on {}", os.name());
        results.push(OpResult::failed(name, code::OP_NOT_SUPPORTED, message));
    }
}

/// What `uninstall_all` runs: the reverse of the setup order, so the `pf`
/// redirect goes before the DNS setup, and the trust goes last.
fn removals(os: Os) -> Vec<Op> {
    match os {
        Os::Macos => vec![Op::PfRemove {}, Op::ResolverRemove {}, Op::TrustRemove {}],
        Os::Linux => vec![Op::ResolvedRemove {}, Op::TrustRemove {}],
        Os::Windows => vec![Op::HostsRemove {}],
    }
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
        Op::ResolvedInstall { .. } | Op::ResolvedRemove {} => os == Os::Linux,
        Op::HostsSync { .. } | Op::HostsRemove {} => os == Os::Windows,
    }
}

/// One op meant for this OS. Each handler comes with its task (M4-07 to
/// M4-09) and gets `system` for every file and command.
fn handle(op: &Op, _system: &impl System) -> OpResult {
    not_implemented(op.name())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use serde_json::{Value, json};

    use super::*;
    use crate::request::Request;
    use crate::response::Outcome;
    use crate::system::FakeSystem;

    /// The op name and the failure code of each result. The request goes
    /// through `from_arg`, like a real one.
    fn codes(ops: Value, os: Os) -> Vec<(&'static str, &'static str)> {
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let fake = FakeSystem::default();
        let results = apply(&request, os, &fake);
        assert!(fake.log.borrow().is_empty());
        let code = |r: &OpResult| match r.outcome {
            Outcome::Failed { code, .. } => code,
            Outcome::Done | Outcome::Unchanged => "",
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
    fn each_os_supports_only_its_own_ops() {
        let table = [
            (Os::Macos, &["trust", "resolver", "pf"][..]),
            (Os::Linux, &["trust", "resolved"]),
            (Os::Windows, &["hosts"]),
        ];
        for (os, own) in table {
            for (name, code) in codes(every_op(), os) {
                let prefix = name.rsplit_once('_').unwrap().0;
                let expected = if own.contains(&prefix) {
                    code::OP_NOT_IMPLEMENTED
                } else {
                    code::OP_NOT_SUPPORTED
                };
                assert_eq!(code, expected, "{name} on {os:?}");
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
            (Os::Linux, &["resolved_remove", "trust_remove"]),
            (Os::Windows, &["hosts_remove"]),
        ];
        for (os, expected) in table {
            let results = codes(json!([{ "op": "uninstall_all" }]), os);
            let names: Vec<_> = results.iter().map(|(name, _)| *name).collect();
            assert_eq!(names, expected, "{os:?}");
            assert!(results.iter().all(|(_, c)| *c == code::OP_NOT_IMPLEMENTED));
        }
    }
}
