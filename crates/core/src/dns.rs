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

    #[test]
    fn any_is_not_a_system() {
        assert!(strategy_for(Platform::Any).is_none());
    }
}
