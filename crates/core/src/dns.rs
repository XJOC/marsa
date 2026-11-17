//! The `DnsStrategy` trait and one implementation per OS: a resolver file on
//! macOS, systemd-resolved on Linux, the hosts file on Windows.
//!
//! Every system sends `.test` names to `127.0.0.1` in its own way:
//!
//! | System | How | Helper needed |
//! | --- | --- | --- |
//! | macOS | `/etc/resolver/test` points `.test` at the DNS server of `marsad` | once, at setup |
//! | Linux | systemd-resolved sends `~test` to the same server | once, at setup |
//! | Windows | one hosts file line per site | on every site change |
//!
//! The daemon asks the strategy what to send to the helper, so it does not
//! need to know the system. The strategy only returns data.
//!
//! Linux has a second way (ADR 0014, task M4-18k): where systemd-resolved
//! does not run, or where its systemd is older than 250 and
//! systemd-networkd may run, the `marsa0` link cannot be used, and each
//! site gets a hosts file line, as on Windows. [`linux_path`] picks the way
//! from what a user can read; `marsa-helper` keeps its own copy of the
//! version rule and refuses the link where it does not hold.

use std::collections::BTreeSet;

use crate::helper::HelperOp;
use crate::recipes::Platform;
use crate::sites::Site;

/// The port of the DNS server in `marsad`, on `127.0.0.1`. Not 53: that
/// needs root, and Herd's `dnsmasq` holds `127.0.0.1:53` on some Macs.
pub const DNS_PORT: u16 = 18053;

/// What the helper must do so that `<site>.test` reaches `127.0.0.1`.
pub trait DnsStrategy {
    /// What the helper does once, at first setup. The setup flow puts it in
    /// one request with the other setup operations, so the user sees one
    /// admin prompt.
    fn setup_op(&self) -> HelperOp;

    /// What the helper does after the site list changed. Only Windows needs
    /// this (the hosts file has no wildcards); macOS and Linux return `None`.
    fn on_sites_changed(&self, sites: &[Site]) -> Option<HelperOp>;

    /// What the helper undoes at uninstall.
    fn remove_op(&self) -> HelperOp;
}

/// macOS: a resolver file for the `test` domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverFile {
    pub port: u16,
}

impl DnsStrategy for ResolverFile {
    fn setup_op(&self) -> HelperOp {
        HelperOp::ResolverInstall { port: self.port }
    }

    fn on_sites_changed(&self, _sites: &[Site]) -> Option<HelperOp> {
        None
    }

    fn remove_op(&self) -> HelperOp {
        HelperOp::ResolverRemove
    }
}

/// Linux: systemd-resolved sends the `~test` domain to our DNS server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemdResolved {
    pub port: u16,
}

impl DnsStrategy for SystemdResolved {
    fn setup_op(&self) -> HelperOp {
        HelperOp::ResolvedInstall { port: self.port }
    }

    fn on_sites_changed(&self, _sites: &[Site]) -> Option<HelperOp> {
        None
    }

    fn remove_op(&self) -> HelperOp {
        HelperOp::ResolvedRemove
    }
}

/// Windows: one hosts file line per site, in Marsa's own marked block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostsFile;

impl DnsStrategy for HostsFile {
    /// An empty block, so the helper service checks it can write the file.
    fn setup_op(&self) -> HelperOp {
        HelperOp::HostsSync { names: Vec::new() }
    }

    /// The host names of every site, sorted, each once.
    fn on_sites_changed(&self, sites: &[Site]) -> Option<HelperOp> {
        let names: BTreeSet<String> = sites.iter().map(Site::host).collect();
        Some(HelperOp::HostsSync {
            names: names.into_iter().collect(),
        })
    }

    fn remove_op(&self) -> HelperOp {
        HelperOp::HostsRemove
    }
}

/// How `.test` names reach `127.0.0.1` on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxPath {
    /// The `marsa0` link: systemd-resolved sends `~test` to [`DNS_PORT`].
    /// Set up once (`resolved_install`).
    Link,
    /// One hosts file line per site, in Marsa's block (`hosts_sync`). The
    /// block changes with every new site.
    Hosts,
}

/// What a user can read of the system, for [`linux_path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxFacts {
    /// The first number of `systemctl --version`; `None` if it could not
    /// be read.
    pub systemd: Option<u32>,
    /// Whether systemd-resolved runs (`systemctl is-active`); `None` if
    /// that could not be read.
    pub resolved_runs: Option<bool>,
    /// Whether systemd-networkd may run, now or at the next boot
    /// ([`networkd_may_run`]). Only systemd 249 asks.
    pub networkd_may_run: bool,
}

/// The first systemd whose resolved keeps a new link's settings while
/// systemd-networkd looks at it (ADR 0014, finding 30).
pub const FIRST_LINK_SYSTEMD: u32 = 250;
/// Before 250 the link is used on 249 only, the version tested (spike
/// S-02c), and only without systemd-networkd.
pub const LINK_BEFORE_250: u32 = 249;

/// The way for `facts`. The same rule as `resolved_install` in
/// `marsa-helper`, which stays the judge: an unknown version means the
/// link, so the helper says what it could not read
/// (`resolved_systemd_unknown`). Hosts lines work with any resolver, so a
/// system where systemd-resolved does not run (Debian 12 by default) gets
/// them too.
pub fn linux_path(facts: LinuxFacts) -> LinuxPath {
    if facts.resolved_runs == Some(false) {
        return LinuxPath::Hosts;
    }
    match facts.systemd {
        None => LinuxPath::Link,
        Some(version) if version >= FIRST_LINK_SYSTEMD => LinuxPath::Link,
        Some(version) if version >= LINK_BEFORE_250 && !facts.networkd_may_run => LinuxPath::Link,
        Some(_) => LinuxPath::Hosts,
    }
}

/// The systemd-networkd units whose state [`networkd_may_run`] reads, in
/// this order. `systemd-networkd-wait-online.service` requires networkd:
/// enabled, it starts networkd at boot.
pub const NETWORKD_UNITS: [&str; 3] = [
    "systemd-networkd.service",
    "systemd-networkd.socket",
    "systemd-networkd-wait-online.service",
];

/// The version in the first line of `systemctl --version`
/// (`systemd 255 (255.4-1ubuntu8)`).
pub fn systemd_version(stdout: &str) -> Option<u32> {
    let first = stdout.lines().next()?;
    first
        .trim_end_matches('\r')
        .strip_prefix("systemd ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Whether systemd-networkd may run, from the output of `systemctl
/// is-active` and `systemctl is-enabled` for [`NETWORKD_UNITS`], one line
/// per unit (`None`: the command did not run to its end). `false` only when
/// all three are `inactive`, and all three `disabled` or `masked`: an
/// enabled networkd starts again at boot or through its socket, and
/// `masked-runtime` is gone after a reboot. Anything else may run.
pub fn networkd_may_run(active: Option<&str>, enabled: Option<&str>) -> bool {
    let states = |text: Option<&str>| -> Option<Vec<String>> {
        let states: Vec<String> = text?.lines().map(|line| line.trim().to_owned()).collect();
        (states.len() == NETWORKD_UNITS.len()).then_some(states)
    };
    let inactive = states(active).is_some_and(|states| states.iter().all(|s| s == "inactive"));
    let off = states(enabled).is_some_and(|states| {
        states
            .iter()
            .all(|state| matches!(state.as_str(), "disabled" | "masked"))
    });
    !(inactive && off)
}

/// The first line of Marsa's block in a hosts file.
pub const HOSTS_BEGIN: &str = "# Marsa start";
/// The last line of Marsa's block in a hosts file.
pub const HOSTS_END: &str = "# Marsa end";

/// The names in Marsa's block of a hosts file: the second field of each
/// line of the block that maps `127.0.0.1`. `None` when the file has no
/// block, or markers that do not pair up: `marsa-helper` leaves such a file
/// as it is, so it counts as no block. Read as `marsa-helper` writes it: a
/// marker is a whole line, apart from spaces, tabs, and `\r` at its end.
pub fn hosts_block_names(text: &str) -> Option<BTreeSet<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut names = BTreeSet::new();
    let mut inside = false;
    let mut found = false;
    for line in text.lines() {
        match line.trim_end_matches([' ', '\t', '\r']) {
            HOSTS_BEGIN if !inside => {
                inside = true;
                found = true;
            }
            HOSTS_END if inside => inside = false,
            HOSTS_BEGIN | HOSTS_END => return None,
            line if inside => {
                let mut fields = line.split_whitespace();
                if fields.next() == Some("127.0.0.1") {
                    names.extend(fields.next().map(str::to_owned));
                }
            }
            _ => {}
        }
    }
    (found && !inside).then_some(names)
}

/// The strategy for a platform, with the DNS server on [`DNS_PORT`].
/// `Platform::Any` is not a system, so it has none.
pub fn strategy_for(platform: Platform) -> Option<Box<dyn DnsStrategy>> {
    match platform {
        Platform::DarwinArm64 | Platform::DarwinX64 => {
            Some(Box::new(ResolverFile { port: DNS_PORT }))
        }
        Platform::LinuxX64 | Platform::LinuxArm64 => {
            Some(Box::new(SystemdResolved { port: DNS_PORT }))
        }
        Platform::WindowsX64 => Some(Box::new(HostsFile)),
        Platform::Any => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sites::SiteKind;
    use std::path::PathBuf;

    fn site(name: &str) -> Site {
        Site {
            name: name.to_string(),
            root: PathBuf::from("/code").join(name),
            docroot: PathBuf::from("/code").join(name),
            kind: SiteKind::Static,
            php: None,
            php_source: None,
            secure: true,
        }
    }

    fn strategy(platform: Platform) -> Box<dyn DnsStrategy> {
        strategy_for(platform).expect("a real system has a strategy")
    }

    #[test]
    fn macos_writes_a_resolver_file_once() {
        for platform in [Platform::DarwinArm64, Platform::DarwinX64] {
            let dns = strategy(platform);
            assert_eq!(dns.setup_op(), HelperOp::ResolverInstall { port: 18053 });
            assert_eq!(dns.on_sites_changed(&[site("shop")]), None);
            assert_eq!(dns.remove_op(), HelperOp::ResolverRemove);
        }
    }

    #[test]
    fn linux_uses_systemd_resolved_once() {
        for platform in [Platform::LinuxX64, Platform::LinuxArm64] {
            let dns = strategy(platform);
            assert_eq!(dns.setup_op(), HelperOp::ResolvedInstall { port: 18053 });
            assert_eq!(dns.on_sites_changed(&[site("shop")]), None);
            assert_eq!(dns.remove_op(), HelperOp::ResolvedRemove);
        }
    }

    #[test]
    fn windows_syncs_hosts_on_every_change() {
        let dns = strategy(Platform::WindowsX64);
        assert_eq!(dns.setup_op(), HelperOp::HostsSync { names: vec![] });
        assert_eq!(dns.remove_op(), HelperOp::HostsRemove);

        let sites = [site("shop"), site("blog"), site("shop")];
        assert_eq!(
            dns.on_sites_changed(&sites),
            Some(HelperOp::HostsSync {
                names: vec!["blog.test".to_string(), "shop.test".to_string()],
            })
        );
        assert_eq!(
            dns.on_sites_changed(&[]),
            Some(HelperOp::HostsSync { names: vec![] })
        );
    }

    fn facts(systemd: Option<u32>, resolved_runs: Option<bool>, networkd: bool) -> LinuxFacts {
        LinuxFacts {
            systemd,
            resolved_runs,
            networkd_may_run: networkd,
        }
    }

    /// The truth table of `resolved_install` in `marsa-helper` (ADR 0014),
    /// and the hosts file wherever systemd-resolved does not run.
    #[test]
    fn linux_takes_the_link_where_the_helper_makes_it() {
        use LinuxPath::{Hosts, Link};
        let table = [
            (facts(Some(255), Some(true), true), Link),
            (facts(Some(250), Some(true), true), Link),
            (facts(Some(249), Some(true), false), Link),
            (facts(Some(249), Some(true), true), Hosts),
            (facts(Some(248), Some(true), false), Hosts),
            (facts(Some(247), None, false), Hosts),
            // The helper says what it could not read.
            (facts(None, Some(true), true), Link),
            (facts(None, None, true), Link),
            (facts(Some(255), None, true), Link),
            // Hosts lines need no resolver at all.
            (facts(Some(255), Some(false), false), Hosts),
            (facts(None, Some(false), false), Hosts),
        ];
        for (facts, path) in table {
            assert_eq!(linux_path(facts), path, "{facts:?}");
        }
    }

    #[test]
    fn the_systemd_version_is_the_first_number() {
        assert_eq!(
            systemd_version("systemd 255 (255.4-1ubuntu8)\n+PAM +AUDIT\n"),
            Some(255)
        );
        assert_eq!(
            systemd_version("systemd 249 (249.11-0ubuntu3.12)\r\n"),
            Some(249)
        );
        assert_eq!(systemd_version("systemd 262\n"), Some(262));
        for bad in [
            "",
            "\n",
            "systemd\n",
            "systemd x (1)\n",
            "launchd 1\n",
            " systemd 255\n",
        ] {
            assert_eq!(systemd_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn networkd_is_off_only_when_all_three_units_are_down_for_good() {
        let off = "disabled\nmasked\ndisabled\n";
        let down = "inactive\ninactive\ninactive\n";
        assert!(!networkd_may_run(Some(down), Some(off)));
        assert!(!networkd_may_run(
            Some(down),
            Some("masked\nmasked\nmasked")
        ));
        for active in [
            "active\ninactive\ninactive\n",
            "inactive\nactivating\ninactive\n",
            "inactive\ninactive\nfailed\n",
            "inactive\ninactive\n",
            "",
        ] {
            assert!(networkd_may_run(Some(active), Some(off)), "{active:?}");
        }
        for enabled in [
            "enabled\ndisabled\ndisabled\n",
            "disabled\ndisabled\nmasked-runtime\n",
            "disabled\nstatic\ndisabled\n",
            "disabled\ndisabled\n",
        ] {
            assert!(networkd_may_run(Some(down), Some(enabled)), "{enabled:?}");
        }
        assert!(networkd_may_run(None, Some(off)));
        assert!(networkd_may_run(Some(down), None));
    }

    #[test]
    fn the_block_gives_its_names() {
        let names = |list: &[&str]| -> Option<BTreeSet<String>> {
            Some(list.iter().map(|name| (*name).to_owned()).collect())
        };
        let debian = "127.0.0.1\tlocalhost\n127.0.1.1\trunner\n";
        assert_eq!(hosts_block_names(debian), None);
        let block = "# Marsa start\n# Managed by Marsa. Lines in this block are overwritten.\n\
                     127.0.0.1 blog.test\n127.0.0.1 shop.test\n# Marsa end\n";
        assert_eq!(
            hosts_block_names(&format!("{debian}{block}")),
            names(&["blog.test", "shop.test"])
        );
        let empty = "# Marsa start\n# Managed by Marsa. Lines in this block are overwritten.\n# Marsa end\n";
        assert_eq!(hosts_block_names(empty), names(&[]));
        // Windows line endings, a byte order mark, spaces after a marker,
        // and lines another tool put in.
        let crlf = "\u{feff}# Marsa start \r\n127.0.0.1   a.test\r\n::1 b.test\r\n\t127.0.0.1\tc.test # x\r\n# Marsa end\t\r\n";
        assert_eq!(hosts_block_names(crlf), names(&["a.test", "c.test"]));
        // Names outside the block are not Marsa's.
        assert_eq!(
            hosts_block_names(&format!("127.0.0.1 x.test\n{block}127.0.0.1 y.test\n")),
            names(&["blog.test", "shop.test"])
        );
        // Markers that do not pair up: the helper leaves the file alone.
        for broken in [
            "# Marsa start\n127.0.0.1 a.test\n",
            "127.0.0.1 a.test\n# Marsa end\n",
            "# Marsa start\n# Marsa start\n# Marsa end\n",
            "# Marsa start\n# Marsa end\n# Marsa end\n",
            "# Marsa end\n# Marsa start\n",
        ] {
            assert_eq!(hosts_block_names(broken), None, "{broken:?}");
        }
        // A marker with something before it is an ordinary line.
        assert_eq!(hosts_block_names("127.0.0.1 x # Marsa start\n"), None);
    }

    #[test]
    fn any_is_not_a_system() {
        assert!(strategy_for(Platform::Any).is_none());
    }
}
