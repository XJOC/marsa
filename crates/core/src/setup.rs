//! First-time setup as a pure plan (ADR 0026): the daemon gathers the
//! [`Facts`] ([`needs`] says which), [`plan`] turns them into the helper's
//! first request, the steps left out, and the steps the daemon does itself,
//! and [`after_helper`] plans the second request on Linux from the helper's
//! answer to the first. Nothing here runs a program or reads a file, so a
//! snapshot answers "what does setup do on this system?".
//!
//! The helper stays the judge (ADR 0014): the second request depends on its
//! answer, so it is a function of the facts and of the results, not of a
//! guess made in advance.

use crate::codes;
use crate::dns::{self, DNS_PORT, LinuxFacts, LinuxPath};
use crate::helper::{self, HelperOp, OpResult, OpStatus};
use crate::recipes::{Os, Platform};
use crate::types::{OpName, PortHolder, SetupSkip};

/// The helper's answers to `resolved_install` that mean "use the hosts file
/// here".
const HOSTS_ANSWERS: [&str; 2] = [codes::RESOLVED_USE_HOSTS, codes::RESOLVED_NOT_RUNNING];

/// Which facts the daemon gathers on a system, so it never asks "which
/// system is this?" itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    /// Who listens on ports 80 and 443 ([`Facts::port_holders`]).
    pub port_holders: bool,
    /// What systemd-resolved says ([`Facts::linux_dns`]).
    pub linux_dns: bool,
    /// The host names of the sites ([`Facts::names`]).
    pub site_names: bool,
    /// Whether Marsa's block is in the hosts file ([`Facts::hosts_block`]).
    pub hosts_block: bool,
}

/// The facts setup needs on `os`.
pub fn needs(os: Os) -> Needs {
    Needs {
        port_holders: os == Os::Mac,
        linux_dns: os == Os::Linux,
        site_names: matches!(os, Os::Linux | Os::Windows),
        hosts_block: os == Os::Linux,
    }
}

/// Everything the daemon found before setup. These are results only:
/// gathering (programs, files, Caddy's start) stays in the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub platform: Platform,
    /// The checked PEM of the CA root, or the code of why there is none
    /// (`ca_unreadable`, `caddy_not_installed`, ...).
    pub ca: Result<String, &'static str>,
    /// The ports Caddy listens on: `(http, https)`.
    pub caddy_ports: (u16, u16),
    /// macOS: the programs on ports 80 and 443; `None` if they could not be
    /// checked.
    pub port_holders: Option<Vec<PortHolder>>,
    /// Linux: what systemd-resolved says; `None` if not read, which counts
    /// as "ask the helper".
    pub linux_dns: Option<LinuxFacts>,
    /// Linux: Marsa's block is in the hosts file now.
    pub hosts_block: bool,
    /// The host names of the sites, sorted; `None` if the list is not known.
    pub names: Option<Vec<String>>,
    /// Why the daemon's DNS server does not answer (`DnsService::not_serving`);
    /// the DNS step is then left out with that code.
    pub dns_down: Option<String>,
    /// Linux: whether setup adds the CA root to the user's NSS databases.
    pub nss: bool,
}

/// What setup does, in the order it does it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The helper's first request. Empty: no prompt.
    pub ops: Vec<HelperOp>,
    /// The steps left out of the request, and why, in order.
    pub skips: Vec<SetupSkip>,
    /// macOS: the programs that hold port 80 or 443, when `pf_install` is
    /// left out because of them.
    pub port_holders: Vec<PortHolder>,
    /// What the daemon does itself, in order.
    pub local: Vec<LocalStep>,
}

/// A step the daemon does as the user, not the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalStep {
    /// macOS: the user's own trust in the CA root (ADR 0015). Only if the
    /// helper ran as asked or was not needed.
    UserTrust { pem: String },
    /// Linux: the CA root in the user's NSS databases (ADR 0014). The same
    /// condition as [`LocalStep::UserTrust`].
    NssInstall { pem: String },
    /// Linux: record the way `.test` names go. Always.
    RecordDnsWay,
}

impl Plan {
    fn skip(&mut self, op: OpName, code: &str) {
        self.skips.push(skip(op, code));
    }
}

fn skip(op: OpName, code: &str) -> SetupSkip {
    SetupSkip {
        op,
        code: code.to_owned(),
    }
}

/// The plan for `facts`.
pub fn plan(facts: &Facts) -> Plan {
    let os = facts.platform.os();
    let mut plan = Plan::default();
    // No build for this system: nothing to plan (the daemon refuses it first).
    if os == Os::Any {
        return plan;
    }
    match &facts.ca {
        Err(code) => plan.skip(OpName::TrustInstall, code),
        // macOS: the user's own trust, after the helper (ADR 0015). Elsewhere
        // the helper trusts the CA as root (ADR 0011).
        Ok(pem) if os == Os::Mac => plan.local.push(LocalStep::UserTrust { pem: pem.clone() }),
        // Linux: the system store through the helper, then the user's NSS
        // databases as the user (ADR 0014).
        Ok(pem) if os == Os::Linux => {
            if facts.nss {
                plan.local.push(LocalStep::NssInstall { pem: pem.clone() });
            }
            plan.ops.push(HelperOp::TrustInstall { pem: pem.clone() });
        }
        // Windows: the service is installed for this root's SHA-256 (ADR
        // 0018). A root the helper's strict parser refuses has none, and
        // every setup would ask UAC in vain.
        Ok(pem) if os == Os::Windows && helper::certificate_sha256(pem).is_none() => {
            plan.skip(OpName::TrustInstall, codes::CA_UNREADABLE);
        }
        Ok(pem) => plan.ops.push(HelperOp::TrustInstall { pem: pem.clone() }),
    }
    match os {
        Os::Linux => linux_dns_step(facts, &mut plan),
        // Windows: the hosts block with the names of the sites now, so that
        // a setup again does not empty it (ADR 0018).
        Os::Windows => {
            let (ops, skipped) = hosts_sync(facts.names.as_deref());
            plan.ops.extend(ops);
            if let Some(code) = skipped {
                plan.skip(OpName::HostsSync, code);
            }
        }
        Os::Mac => match &facts.dns_down {
            Some(code) => plan.skip(OpName::ResolverInstall, code),
            None => plan.ops.extend(dns::setup_op(facts.platform)),
        },
        Os::Any => unreachable!("returned above"),
    }
    match os {
        Os::Linux => plan.ops.push(HelperOp::PortsInstall),
        Os::Mac => pf_step(facts, &mut plan),
        // Windows: Caddy binds ports 80 and 443 itself (ADR 0018), so there
        // is nothing to redirect and nothing to allow.
        Os::Windows | Os::Any => {}
    }
    plan
}

/// Linux: the operation of the way that [`dns::linux_path`] gives, and the
/// step that records it.
fn linux_dns_step(facts: &Facts, plan: &mut Plan) {
    match (linux_path(facts), &facts.dns_down) {
        (LinuxPath::Link, Some(code)) => plan.skip(OpName::ResolvedInstall, code),
        (LinuxPath::Link, None) => plan.ops.push(HelperOp::ResolvedInstall { port: DNS_PORT }),
        (LinuxPath::Hosts, _) => {
            let (ops, skipped) = hosts_way(facts.names.as_deref());
            plan.ops.extend(ops);
            if let Some(code) = skipped {
                plan.skip(OpName::HostsSync, code);
            }
        }
    }
    plan.local.push(LocalStep::RecordDnsWay);
}

/// The way of `.test` names on Linux. Facts that were not read mean "ask
/// the helper": the link.
fn linux_path(facts: &Facts) -> LinuxPath {
    dns::linux_path(facts.linux_dns.unwrap_or(LinuxFacts {
        systemd: None,
        resolved_runs: None,
        networkd_may_run: false,
    }))
}

/// macOS: `pf_install` if nobody listens on 80 and 443, else the reason why
/// not (ADR 0008, "Herd conflict").
fn pf_step(facts: &Facts, plan: &mut Plan) {
    match &facts.port_holders {
        Some(holders) if holders.is_empty() => plan.ops.push(HelperOp::PfInstall {
            http_port: facts.caddy_ports.0,
            https_port: facts.caddy_ports.1,
        }),
        Some(holders) => {
            plan.port_holders.clone_from(holders);
            plan.skip(OpName::PfInstall, codes::REDIRECT_PORT_HELD);
        }
        None => plan.skip(OpName::PfInstall, codes::PORTS_NOT_CHECKED),
    }
}

/// `hosts_sync` with `names`, the host names of the sites (`None`: not
/// known). The second value is why it was left out: an empty list would
/// empty the block, and a list past the helper's limits would make it
/// refuse the whole request, the certificate and the ports with it.
fn hosts_sync(names: Option<&[String]>) -> (Option<HelperOp>, Option<&'static str>) {
    match names {
        None => (None, Some(codes::HOSTS_SITES_UNKNOWN)),
        Some(names) if !dns::hosts_names_fit(names) => (None, Some(codes::HOSTS_TOO_MANY_NAMES)),
        Some(names) => (
            Some(HelperOp::HostsSync {
                names: names.to_vec(),
            }),
            None,
        ),
    }
}

/// The operations of the hosts way on Linux: `resolved_remove`, which takes
/// out a link of an earlier setup (it would meet the race of ADR 0014,
/// finding 30, at every boot; with no link it changes nothing), then
/// `hosts_sync`.
fn hosts_way(names: Option<&[String]>) -> (Vec<HelperOp>, Option<&'static str>) {
    let (sync, skipped) = hosts_sync(names);
    let ops = std::iter::once(HelperOp::ResolvedRemove)
        .chain(sync)
        .collect();
    (ops, skipped)
}

/// Linux: what setup sends after the helper's answer to the first request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUp {
    /// The second request. Empty: none is sent, and there is no second
    /// prompt; `retire` and `skips` still apply.
    pub ops: Vec<HelperOp>,
    /// The helper chose the hosts file: this operation's result moves into
    /// the skipped steps, with the helper's code.
    pub retire: Option<(OpName, String)>,
    /// Steps left out of the second request.
    pub skips: Vec<SetupSkip>,
    /// The error code if the second request does not run: the steps before
    /// it are done ([`codes::SETUP_HOSTS_NOT_WRITTEN`] or
    /// [`codes::SETUP_HOSTS_NOT_REMOVED`]).
    pub not_run: &'static str,
}

/// Linux, after a first request with `resolved_install`, and only if the
/// helper answered it. The helper is the judge of the way:
/// - If it chose the hosts file ([`helper_chose_hosts`]), `resolved_install`
///   moves from the results to the skipped steps, and the operations of the
///   hosts way go in a second request.
/// - If it made the link and the hosts file still has Marsa's block,
///   `hosts_remove` goes in a second request.
///
/// Either is a second password prompt of the same `marsa setup`.
pub fn after_helper(facts: &Facts, results: &[OpResult]) -> Option<FollowUp> {
    if facts.platform.os() != Os::Linux || linux_path(facts) != LinuxPath::Link {
        return None;
    }
    if let Some(code) = helper_chose_hosts(results) {
        let (mut ops, skipped) = hosts_way(facts.names.as_deref());
        // No names to write, no prompt: after `resolved_use_hosts` the
        // helper took an earlier link out itself.
        if !ops
            .iter()
            .any(|op| matches!(op, HelperOp::HostsSync { .. }))
        {
            ops.clear();
        }
        Some(FollowUp {
            ops,
            retire: Some((OpName::ResolvedInstall, code)),
            skips: skipped
                .map(|code| skip(OpName::HostsSync, code))
                .into_iter()
                .collect(),
            not_run: codes::SETUP_HOSTS_NOT_WRITTEN,
        })
    } else if facts.hosts_block && link_made(results) {
        Some(FollowUp {
            ops: vec![HelperOp::HostsRemove],
            retire: None,
            skips: Vec::new(),
            not_run: codes::SETUP_HOSTS_NOT_REMOVED,
        })
    } else {
        None
    }
}

/// Whether the helper made the `marsa0` link, now or before.
pub fn link_made(results: &[OpResult]) -> bool {
    results.iter().any(|result| {
        result.op == OpName::ResolvedInstall
            && matches!(result.status, OpStatus::Done | OpStatus::Unchanged)
    })
}

/// The code of a failed `resolved_install` that means the helper chose the
/// hosts file: `resolved_use_hosts` (its systemd rule) or
/// `resolved_not_running`.
fn helper_chose_hosts(results: &[OpResult]) -> Option<String> {
    results
        .iter()
        .filter(|result| result.op == OpName::ResolvedInstall && result.status == OpStatus::Failed)
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
    use std::path::Path;

    use super::*;

    fn pem() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("helper")
            .join("test-ca.pem");
        fs::read_to_string(path).unwrap()
    }

    fn names(list: &[&str]) -> Option<Vec<String>> {
        Some(list.iter().map(|name| (*name).to_owned()).collect())
    }

    fn holder(port: u16) -> PortHolder {
        PortHolder {
            port,
            address: "*".to_owned(),
            process: Some("nginx".to_owned()),
            pid: Some(77),
            user: Some("omar".to_owned()),
            command: None,
            app: None,
        }
    }

    fn mac() -> Facts {
        Facts {
            platform: Platform::DarwinArm64,
            ca: Ok("PEM".to_owned()),
            caddy_ports: (8080, 8443),
            port_holders: Some(Vec::new()),
            linux_dns: None,
            hosts_block: false,
            names: Some(Vec::new()),
            dns_down: None,
            nss: false,
        }
    }

    fn linux(systemd: Option<u32>, resolved_runs: Option<bool>) -> Facts {
        Facts {
            platform: Platform::LinuxX64,
            port_holders: None,
            linux_dns: Some(LinuxFacts {
                systemd,
                resolved_runs,
                networkd_may_run: false,
            }),
            names: names(&["a.test", "b.test"]),
            nss: true,
            ..mac()
        }
    }

    fn windows() -> Facts {
        Facts {
            platform: Platform::WindowsX64,
            ca: Ok(pem()),
            port_holders: None,
            names: names(&["a.test"]),
            ..mac()
        }
    }

    fn result(op: OpName, status: OpStatus, code: Option<&str>) -> OpResult {
        OpResult {
            op,
            status,
            code: code.map(str::to_owned),
            message: None,
        }
    }

    #[test]
    fn each_system_gathers_only_its_own_facts() {
        assert_eq!(
            needs(Os::Mac),
            Needs {
                port_holders: true,
                ..Needs::default()
            }
        );
        assert_eq!(
            needs(Os::Linux),
            Needs {
                linux_dns: true,
                site_names: true,
                hosts_block: true,
                ..Needs::default()
            }
        );
        assert_eq!(
            needs(Os::Windows),
            Needs {
                site_names: true,
                ..Needs::default()
            }
        );
        assert_eq!(needs(Os::Any), Needs::default());
    }

    #[test]
    fn macos_with_free_ports() {
        insta::assert_debug_snapshot!(plan(&mac()));
    }

    #[test]
    fn macos_with_a_program_on_the_ports() {
        let facts = Facts {
            port_holders: Some(vec![holder(80), holder(443)]),
            ..mac()
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn macos_with_ports_not_checked() {
        let facts = Facts {
            port_holders: None,
            ..mac()
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn macos_with_dns_down() {
        let facts = Facts {
            dns_down: Some("dns_failed".to_owned()),
            ..mac()
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn macos_without_a_ca() {
        let facts = Facts {
            ca: Err(codes::CA_UNREADABLE),
            ..mac()
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn linux_with_the_link() {
        insta::assert_debug_snapshot!(plan(&linux(Some(255), Some(true))));
    }

    /// systemd-resolved does not run: the hosts file, with the names.
    #[test]
    fn linux_with_the_hosts_file() {
        insta::assert_debug_snapshot!(plan(&linux(Some(255), Some(false))));
    }

    #[test]
    fn linux_with_the_hosts_file_and_names_that_do_not_fit() {
        let many: Vec<String> = (0..2000).map(|n| format!("site-{n}.test")).collect();
        let facts = Facts {
            names: Some(many),
            ..linux(Some(255), Some(false))
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn linux_with_the_hosts_file_and_names_unknown() {
        let facts = Facts {
            names: None,
            ..linux(Some(255), Some(false))
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn linux_with_an_old_block_and_the_link() {
        let facts = Facts {
            hosts_block: true,
            ..linux(Some(255), Some(true))
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn linux_without_nss() {
        let facts = Facts {
            nss: false,
            ..linux(Some(255), Some(true))
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn linux_with_dns_down() {
        let facts = Facts {
            dns_down: Some("dns_not_running".to_owned()),
            ..linux(Some(255), Some(true))
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    /// Facts that were not read mean the helper decides.
    #[test]
    fn linux_without_facts_asks_the_helper() {
        let facts = Facts {
            linux_dns: None,
            ..linux(None, None)
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn linux_without_a_ca() {
        let facts = Facts {
            ca: Err(codes::CADDY_NOT_INSTALLED),
            ..linux(Some(255), Some(true))
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    /// The plan on Windows, with the fixture PEM named instead of printed:
    /// its text is not the point of a snapshot.
    fn windows_plan(facts: &Facts) -> Plan {
        let mut plan = plan(facts);
        for op in &mut plan.ops {
            if let HelperOp::TrustInstall { pem } = op {
                "<the fixture PEM>".clone_into(pem);
            }
        }
        plan
    }

    #[test]
    fn windows_with_names_that_fit() {
        insta::assert_debug_snapshot!(windows_plan(&windows()));
    }

    #[test]
    fn windows_with_too_many_names() {
        let many: Vec<String> = (0..2000).map(|n| format!("site-{n}.test")).collect();
        let facts = Facts {
            names: Some(many),
            ..windows()
        };
        insta::assert_debug_snapshot!(windows_plan(&facts));
    }

    #[test]
    fn windows_with_names_unknown() {
        let facts = Facts {
            names: None,
            ..windows()
        };
        insta::assert_debug_snapshot!(windows_plan(&facts));
    }

    /// A root the helper's strict parser refuses: no `trust_install`.
    #[test]
    fn windows_with_a_pem_that_is_not_strict() {
        let facts = Facts {
            ca: Ok("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".to_owned()),
            ..windows()
        };
        insta::assert_debug_snapshot!(plan(&facts));
    }

    #[test]
    fn a_system_with_no_build_has_an_empty_plan() {
        let facts = Facts {
            platform: Platform::Any,
            ..mac()
        };
        assert_eq!(plan(&facts), Plan::default());
    }

    /// The helper chose the hosts file: the second request is the hosts way.
    #[test]
    fn after_helper_when_the_helper_chose_the_hosts_file() {
        let results = [
            result(OpName::TrustInstall, OpStatus::Done, None),
            result(
                OpName::ResolvedInstall,
                OpStatus::Failed,
                Some(codes::RESOLVED_USE_HOSTS),
            ),
            result(OpName::PortsInstall, OpStatus::Done, None),
        ];
        insta::assert_debug_snapshot!(after_helper(&linux(Some(255), Some(true)), &results));
    }

    #[test]
    fn after_helper_when_resolved_is_not_running() {
        let results = [result(
            OpName::ResolvedInstall,
            OpStatus::Failed,
            Some(codes::RESOLVED_NOT_RUNNING),
        )];
        let follow_up = after_helper(&linux(Some(255), Some(true)), &results).unwrap();
        assert_eq!(
            follow_up.retire,
            Some((
                OpName::ResolvedInstall,
                codes::RESOLVED_NOT_RUNNING.to_owned()
            ))
        );
        assert_eq!(follow_up.not_run, codes::SETUP_HOSTS_NOT_WRITTEN);
    }

    /// No names to write: no second request, no prompt, but the link
    /// result still retires.
    #[test]
    fn after_helper_with_the_hosts_file_and_no_names() {
        let results = [result(
            OpName::ResolvedInstall,
            OpStatus::Failed,
            Some(codes::RESOLVED_USE_HOSTS),
        )];
        let facts = Facts {
            names: None,
            ..linux(Some(255), Some(true))
        };
        insta::assert_debug_snapshot!(after_helper(&facts, &results));
    }

    /// The link is made and an old block stays: `hosts_remove`.
    #[test]
    fn after_helper_when_the_link_is_made_and_an_old_block_stays() {
        let results = [result(OpName::ResolvedInstall, OpStatus::Done, None)];
        let facts = Facts {
            hosts_block: true,
            ..linux(Some(255), Some(true))
        };
        insta::assert_debug_snapshot!(after_helper(&facts, &results));
    }

    #[test]
    fn after_helper_has_nothing_to_add_in_the_ordinary_cases() {
        let made = [result(OpName::ResolvedInstall, OpStatus::Done, None)];
        // The link is made and there is no old block.
        assert_eq!(after_helper(&linux(Some(255), Some(true)), &made), None);
        // An old block, but the link failed for another reason.
        let failed = [result(
            OpName::ResolvedInstall,
            OpStatus::Failed,
            Some("resolved_failed"),
        )];
        let facts = Facts {
            hosts_block: true,
            ..linux(Some(255), Some(true))
        };
        assert_eq!(after_helper(&facts, &failed), None);
        // The hosts way: no link was asked for.
        let chose = [result(
            OpName::ResolvedInstall,
            OpStatus::Failed,
            Some(codes::RESOLVED_USE_HOSTS),
        )];
        assert_eq!(after_helper(&linux(Some(255), Some(false)), &chose), None);
        // Not Linux.
        assert_eq!(after_helper(&mac(), &made), None);
    }

    #[test]
    fn link_made_counts_done_and_unchanged_only() {
        for (status, expected) in [
            (OpStatus::Done, true),
            (OpStatus::Unchanged, true),
            (OpStatus::Failed, false),
        ] {
            let results = [result(OpName::ResolvedInstall, status, None)];
            assert_eq!(link_made(&results), expected);
        }
        assert!(!link_made(&[result(
            OpName::HostsSync,
            OpStatus::Done,
            None
        )]));
    }
}
