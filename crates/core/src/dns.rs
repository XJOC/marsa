//! How `.test` names reach `127.0.0.1` on each OS: a resolver file on macOS,
//! systemd-resolved on Linux, the hosts file on Windows.
//!
//! Every system sends `.test` names to `127.0.0.1` in its own way:
//!
//! | System | How | Helper needed |
//! | --- | --- | --- |
//! | macOS | `/etc/resolver/test` points `.test` at the DNS server of `marsad` | once, at setup |
//! | Linux | systemd-resolved sends `~test` to the same server | once, at setup |
//! | Windows | one hosts file line per site | on every site change |
//!
//! The daemon asks [`setup_op`] what to send to the helper at setup, so it
//! does not need to know the system. The function only returns data.
//!
//! Linux has a second way (ADR 0014): where systemd-resolved
//! does not run, or where its systemd is older than 250 and
//! systemd-networkd may run, the `marsa0` link cannot be used, and each
//! site gets a hosts file line, as on Windows. [`linux_path`] picks the way
//! from what a user can read; `marsa-helper` keeps its own copy of the
//! version rule and refuses the link where it does not hold.

use std::collections::BTreeSet;

use crate::helper::HelperOp;
use crate::recipes::Platform;

/// The port of the DNS server in `marsad`, on `127.0.0.1`. Not 53: that
/// needs root, and Herd's `dnsmasq` holds `127.0.0.1:53` on some Macs.
pub const DNS_PORT: u16 = 18053;

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
    /// Whether systemd-resolved runs (`systemctl is-active`, as the helper
    /// reads it); `None` if the read did not end, and the helper is asked.
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
/// them too: there the helper would refuse the link with
/// `resolved_not_running`.
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

/// The most names one `hosts_sync` may carry: `marsa-helper` refuses a
/// request with more (`request.rs`).
pub const MAX_HOSTS_NAMES: usize = 1000;
/// The most bytes the names of one `hosts_sync` may take in the request,
/// with their quotes and commas. The helper reads at most 64 KiB, and a
/// certificate of up to 16 KiB may ride in the same request.
pub const MAX_HOSTS_NAMES_BYTES: usize = 44 * 1024;

/// Whether `names` fit in one `hosts_sync` that the helper accepts
/// ([`MAX_HOSTS_NAMES`], [`MAX_HOSTS_NAMES_BYTES`]). A request the helper
/// refuses would lose its other operations too.
pub fn hosts_names_fit(names: &[String]) -> bool {
    let bytes: usize = names.iter().map(|name| name.len() + 3).sum();
    names.len() <= MAX_HOSTS_NAMES && bytes <= MAX_HOSTS_NAMES_BYTES
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

/// What the helper does once, at first setup, so that `<site>.test` reaches
/// `127.0.0.1`: a resolver file on macOS, systemd-resolved on Linux, an empty
/// hosts block on Windows (so the helper checks it can write the file). The
/// setup flow puts it in one request with the other setup operations, so the
/// user sees one admin prompt. `Platform::Any` is not a system, so it has none.
#[must_use]
pub fn setup_op(platform: Platform) -> Option<HelperOp> {
    match platform {
        Platform::DarwinArm64 | Platform::DarwinX64 => {
            Some(HelperOp::ResolverInstall { port: DNS_PORT })
        }
        Platform::LinuxX64 | Platform::LinuxArm64 => {
            Some(HelperOp::ResolvedInstall { port: DNS_PORT })
        }
        Platform::WindowsX64 => Some(HelperOp::HostsSync { names: Vec::new() }),
        Platform::Any => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(platform: Platform) -> HelperOp {
        setup_op(platform).expect("a real system has a setup op")
    }

    #[test]
    fn macos_writes_a_resolver_file_once() {
        for platform in [Platform::DarwinArm64, Platform::DarwinX64] {
            assert_eq!(op(platform), HelperOp::ResolverInstall { port: 18053 });
        }
    }

    #[test]
    fn linux_uses_systemd_resolved_once() {
        for platform in [Platform::LinuxX64, Platform::LinuxArm64] {
            assert_eq!(op(platform), HelperOp::ResolvedInstall { port: 18053 });
        }
    }

    #[test]
    fn windows_starts_with_an_empty_hosts_block() {
        assert_eq!(
            op(Platform::WindowsX64),
            HelperOp::HostsSync { names: vec![] }
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
            (facts(Some(255), None, true), Link),
            (facts(None, None, true), Link),
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
    fn names_fit_in_one_request_the_helper_takes() {
        let names = |count: usize, len: usize| -> Vec<String> {
            (0..count)
                .map(|i| format!("{:0>width$}.test", i, width = len - 5))
                .collect()
        };
        assert!(hosts_names_fit(&[]));
        assert!(hosts_names_fit(&names(MAX_HOSTS_NAMES, 20)));
        assert!(!hosts_names_fit(&names(MAX_HOSTS_NAMES + 1, 10)));
        // 1000 names of 68 bytes: about 70 KiB, more than the helper reads.
        assert!(!hosts_names_fit(&names(MAX_HOSTS_NAMES, 68)));
        assert!(hosts_names_fit(&names(600, 68)));
        assert!(!hosts_names_fit(&names(700, 68)));
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
        assert!(setup_op(Platform::Any).is_none());
    }
}
