//! Whether a normal process may bind ports 80 and 443 (ADR 0014 for Linux,
//! ADR 0018 for Windows).
//!
//! Linux lets only root bind a port below the sysctl
//! `net.ipv4.ip_unprivileged_port_start`, 1024 by default. The helper's
//! `ports_install` sets it to 80. The daemon asks here, so Caddy takes ports
//! 80 and 443 itself once that is allowed, and stays on its high ports
//! before.
//!
//! Always `true` on Windows: any program may bind a low port there, and
//! nothing redirects, so Caddy takes 80 and 443 from its first start. If
//! another program holds one (IIS holds port 80 on some machines), Caddy's
//! own bind error is the report (`port_in_use`).
//!
//! Always `false` on macOS, where the `pf` redirect brings 80 and 443 to the
//! high ports (ADR 0008).

/// The lowest port Caddy needs: 80 for HTTP. Port 443 is then allowed too.
// Unused outside Linux and the tests, like `parse`.
#[cfg(any(target_os = "linux", test))]
const NEEDED: u32 = 80;

/// Whether this process may bind `127.0.0.1:80` and `:443` now. `false` if
/// the value cannot be read: Caddy then keeps its high ports, which always
/// work.
#[cfg(target_os = "linux")]
pub fn low_ports_allowed() -> bool {
    std::fs::read_to_string("/proc/sys/net/ipv4/ip_unprivileged_port_start")
        .ok()
        .and_then(|text| parse(&text))
        .is_some_and(|lowest| lowest <= NEEDED)
}

/// Windows: always. Low ports need no right there.
#[cfg(windows)]
pub fn low_ports_allowed() -> bool {
    true
}

/// macOS (and any other system): never.
#[cfg(not(any(target_os = "linux", windows)))]
pub fn low_ports_allowed() -> bool {
    false
}

/// The number in the kernel's file: digits and a line break.
#[cfg(any(target_os = "linux", test))]
fn parse(text: &str) -> Option<u32> {
    text.trim_ascii().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_value_is_a_number_with_a_line_break() {
        assert_eq!(parse("1024\n"), Some(1024));
        assert_eq!(parse("80\n"), Some(80));
        assert_eq!(parse("0"), Some(0));
        for bad in ["", "\n", "abc\n", "-1\n", "80 443\n", "8.0\n"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn port_80_is_the_limit() {
        let allowed = |text: &str| parse(text).is_some_and(|lowest| lowest <= NEEDED);
        assert!(allowed("0\n") && allowed("80\n"));
        assert!(!allowed("81\n") && !allowed("1024\n") && !allowed("abc\n"));
    }

    /// Every Linux has the file. The value itself is the machine's own.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_has_the_value() {
        let text = std::fs::read_to_string("/proc/sys/net/ipv4/ip_unprivileged_port_start");
        assert!(parse(&text.unwrap()).is_some());
    }

    #[cfg(windows)]
    #[test]
    fn windows_always_allows_it() {
        assert!(low_ports_allowed());
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    #[test]
    fn other_systems_never_allow_it() {
        assert!(!low_ports_allowed());
    }
}
