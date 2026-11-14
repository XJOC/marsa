//! Who holds a TCP port: reads the output of `netstat` and `ps` on macOS.
//!
//! The `pf` redirect of ADR 0008 sends every connection to `127.0.0.1:80`,
//! `127.0.0.1:443`, `[::1]:80`, and `[::1]:443` to Caddy. Another program that
//! listens on those ports (Laravel Herd's nginx, for example) stops getting
//! its traffic, and neither program logs an error. So the daemon asks who
//! holds the ports before it asks for the redirect, and on every start:
//!
//! 1. `/usr/sbin/netstat -anv -p tcp` lists the TCP sockets of every user,
//!    with a `name:pid` column. [`parse_netstat`] keeps the `LISTEN` rows, and
//!    [`holders`] picks the rows that take loopback connections on one port.
//! 2. `/bin/ps -p <pid> -o user=,comm=` gives the user and the program of one
//!    PID. [`parse_ps`] reads it, and [`known_app`] guesses the app (Herd,
//!    Docker, ...) for the message.
//!
//! The output of `netstat` is not a stable interface, so the parser is
//! defensive: a line it does not understand is skipped, never an error and
//! never a panic. Only macOS is covered. Linux (`/proc/net/tcp` or `ss`) and
//! Windows (`netstat -ano`) come later.
//!
//! Not behind the `full` feature: pure text parsing, no I/O. The daemon runs
//! the two commands (task M4-14b).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// One TCP socket in the `LISTEN` state: one row of `netstat -anv -p tcp`.
///
/// The same process can have several rows: one per address family, and one
/// per port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    /// The local address as netstat prints it, without the port: `127.0.0.1`,
    /// `::1`, or `*` for every address of the family. Text, not an
    /// [`IpAddr`]: netstat cuts long IPv6 addresses to fit its column.
    pub address: String,
    /// The local port.
    pub port: u16,
    /// The process name from the `process:pid` column. The kernel keeps at
    /// most 16 characters (`herd-devtools-he`), and a name can contain spaces
    /// (`Sample Drive`). `None` if the column is missing or the name is empty.
    pub process: Option<String>,
    /// The PID from the same column. When several processes share a socket
    /// (an nginx master and its workers), netstat shows one of them.
    pub pid: Option<u32>,
}

/// The user and the program of one process, from `ps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    /// The user name (`nobody`, `root`), or the user ID if the user has no
    /// name.
    pub user: String,
    /// The program. A path for most programs
    /// (`/Applications/Herd.app/Contents/Resources/dnsmasq-arm64`), but a
    /// process can set its own title (`nginx: worker process`).
    pub command: String,
}

/// Reads the output of `/usr/sbin/netstat -anv -p tcp` and returns the
/// sockets in the `LISTEN` state, in netstat's order: IPv4 (`tcp4`), IPv6
/// (`tcp6`), and dual-stack (`tcp46`) rows.
///
/// A row on macOS 26, cut after the `process:pid` column:
///
/// ```text
/// tcp4       0      0  127.0.0.1.80           *.*                    LISTEN                 0            0  131072  131072      nginx-arm64:15464  00180 ...
/// ```
///
/// The fields, split at whitespace:
///
/// | Field | Example | Use |
/// | --- | --- | --- |
/// | 1 | `tcp4`, `tcp6`, `tcp46` | must start with `tcp` |
/// | 2, 3 | `0` | `Recv-Q` and `Send-Q`: must be numbers |
/// | 4 | `127.0.0.1.80`, `*.443`, `::1.80` | the local address: the port is the part after the last `.` |
/// | 5 | `*.*` | the foreign address: not used |
/// | 6 | `LISTEN` | must be `LISTEN` |
/// | 7 to 10 | `131072` | four counters: skipped |
/// | then | `nginx-arm64:15464`, `Sample Drive:1313` | the process name and the PID |
///
/// Defensive, because the output is not a stable interface:
///
/// - A line that does not fit is skipped: the title, the column header, an
///   empty or cut line, a port that is not a number, a row in another state.
/// - A `\r` at the end of a line is cut.
/// - If only the `process:pid` column cannot be read, the row is still
///   returned, with `process` and `pid` set to `None`: a holder without a
///   name is still a holder.
pub fn parse_netstat(text: &str) -> Vec<Listener> {
    text.lines().filter_map(parse_row).collect()
}

/// The listeners that take connections to `127.0.0.1:<port>` or
/// `[::1]:<port>`, in netstat's order. These are the addresses that the `pf`
/// redirect of ADR 0008 captures. `*.test` resolves to `127.0.0.1` only
/// (task M5-17); `::1` stays here because pf redirects it too.
///
/// - **Loopback:** `127.0.0.1` or `::1` (also written `::ffff:127.0.0.1`).
/// - **Wildcard:** `*` (netstat's form), `0.0.0.0`, or `::`. A wildcard socket
///   takes connections to every local address, loopback included. A loopback
///   bind can sit on top of a wildcard holder without an error (ADR 0008,
///   T11), so a bind test misses it: this check must not.
/// - **Any other address** is not a holder: a LAN address (`192.168.1.5`),
///   another loopback address (`127.0.0.2`), or a link-local address
///   (`fe80::1%lo0`). Connections to `127.0.0.1` and `::1` never reach those
///   sockets, and the redirect rules (for `127.0.0.1` and `::1` only) do not
///   touch their traffic.
///
/// The port must be equal: 8080 and 180 are not 80.
pub fn holders(listeners: &[Listener], port: u16) -> Vec<&Listener> {
    listeners
        .iter()
        .filter(|listener| listener.port == port && takes_loopback(&listener.address))
        .collect()
}

/// Reads the output of `/bin/ps -p <pid> -o user=,comm=`: the user, then the
/// command after the first run of whitespace. The command can contain spaces
/// (`nginx: worker process`). Only the first line that is not empty is read,
/// and whitespace and `\r` around it are cut.
///
/// `None` if the text is empty (the process has ended: `ps` then prints
/// nothing on stdout) or if a line has a user but no command.
pub fn parse_ps(text: &str) -> Option<ProcessInfo> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let (user, command) = line.split_once(char::is_whitespace)?;
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    Some(ProcessInfo {
        user: user.to_string(),
        command: command.to_string(),
    })
}

/// Guesses the app that a holder belongs to, for the message
/// ("Port 80 is used by nginx-arm64 (PID 15464, Herd)").
///
/// A hint only, never a decision: a name can lie, and most apps are not in
/// this list. Try the `ps` command first, then the netstat process name: the
/// nginx worker that netstat names shows only its title in `ps`
/// (`nginx: worker process`), so its name (`nginx-arm64`) is the only hint.
///
/// | App | Matches |
/// | --- | --- |
/// | `Herd` | `Herd` anywhere (`/Applications/Herd.app/...`), a program name that starts with `herd-`, or the programs that Herd ships: `nginx-arm64` and `dnsmasq-arm64` (seen on an arm64 Mac) |
/// | `Docker` | `com.docker` (`com.docker.backend`), `Docker.app`, or `vpnkit` |
/// | `MAMP` | `MAMP` (`/Applications/MAMP/...`, `MAMP PRO.app`) |
/// | `XAMPP` | `XAMPP` or `xampp` (`/Applications/XAMPP/xamppfiles/...`) |
///
/// The program name is the last part of the first word: `nginx-arm64` in
/// `/Applications/Herd.app/Contents/Resources/nginx-arm64 -c nginx.conf`.
pub fn known_app(command: &str) -> Option<&'static str> {
    let program = command
        .split_whitespace()
        .next()
        .and_then(|word| word.rsplit('/').next())
        .unwrap_or_default();
    let has = |part: &str| command.contains(part);

    if has("Herd")
        || program.starts_with("herd-")
        || matches!(program, "nginx-arm64" | "dnsmasq-arm64")
    {
        Some("Herd")
    } else if has("com.docker") || has("Docker.app") || has("vpnkit") {
        Some("Docker")
    } else if has("MAMP") {
        Some("MAMP")
    } else if has("XAMPP") || has("xampp") {
        Some("XAMPP")
    } else {
        None
    }
}

/// Reads one netstat row, or `None` if it does not fit. See [`parse_netstat`].
fn parse_row(line: &str) -> Option<Listener> {
    let line = line.trim_end_matches('\r');
    let fields = fields(line);
    let [
        (_, proto),
        (_, recv_q),
        (_, send_q),
        (_, local),
        _foreign,
        (_, state),
        after_state @ ..,
    ] = fields.as_slice()
    else {
        return None;
    };
    if !proto.starts_with("tcp") || !is_number(recv_q) || !is_number(send_q) || *state != "LISTEN" {
        return None;
    }
    let (address, port) = split_address(local)?;
    let (process, pid) = match process_column(line, after_state) {
        Some((process, pid)) => (process, Some(pid)),
        None => (None, None),
    };
    Some(Listener {
        address: address.to_string(),
        port,
        process,
        pid,
    })
}

/// Splits a line at whitespace, like [`str::split_whitespace`], and keeps the
/// byte offset of each field, so a name with spaces can be cut from the line.
fn fields(line: &str) -> Vec<(usize, &str)> {
    let mut fields = Vec::new();
    let mut start = None;
    for (at, c) in line.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(from)) => {
                fields.push((from, &line[from..at]));
                start = None;
            }
            (false, None) => start = Some(at),
            _ => {}
        }
    }
    if let Some(from) = start {
        fields.push((from, &line[from..]));
    }
    fields
}

/// Splits `127.0.0.1.80` into `127.0.0.1` and 80, and `*.443` into `*` and
/// 443. The port is the part after the last `.`, and it must be digits
/// only: `*.*` gives `None`.
fn split_address(local: &str) -> Option<(&str, u16)> {
    let (host, port) = local.rsplit_once('.')?;
    if host.is_empty() || !is_number(port) {
        return None;
    }
    Some((host, port.parse().ok()?))
}

/// Reads the `process:pid` column from the fields after the state.
///
/// The four counters come first, then the column. The name is right-aligned
/// and can contain spaces (`Sample Drive:1313`, `Sample Code IDE :13143`), so
/// it is cut from the line itself: from the first field after the counters
/// to the last `:` of the first field that ends in `:<digits>`. The counters
/// never contain a `:`, so they cannot be taken for that field.
///
/// `None` if no field ends in `:<digits>`. The name is `None` if it is empty.
fn process_column(line: &str, after_state: &[(usize, &str)]) -> Option<(Option<String>, u32)> {
    let (at, colon, pid) = after_state
        .iter()
        .enumerate()
        .find_map(|(at, &(start, field))| {
            split_pid(field).map(|(colon, pid)| (at, start + colon, pid))
        })?;
    let counters = after_state
        .iter()
        .take(at)
        .take_while(|(_, field)| is_number(field))
        .count();
    let name_start = after_state.get(counters).map_or(colon, |&(start, _)| start);
    let process = line
        .get(name_start..colon)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    Some((process, pid))
}

/// `nginx-arm64:15464` → the byte offset of the last `:`, and 15464.
fn split_pid(field: &str) -> Option<(usize, u32)> {
    let colon = field.rfind(':')?;
    let pid = field.get(colon + 1..)?;
    if !is_number(pid) {
        return None;
    }
    Some((colon, pid.parse().ok()?))
}

/// One or more ASCII digits. [`str::parse`] alone also takes `+80`.
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// True for an address that takes connections to `127.0.0.1` or `::1`. See
/// [`holders`] for the rule.
fn takes_loopback(address: &str) -> bool {
    let address = address.trim_start_matches('[').trim_end_matches(']');
    if address == "*" {
        return true;
    }
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip == Ipv4Addr::LOCALHOST || ip.is_unspecified(),
        Ok(IpAddr::V6(ip)) => {
            ip == Ipv6Addr::LOCALHOST
                || ip.is_unspecified()
                || ip.to_ipv4_mapped() == Some(Ipv4Addr::LOCALHOST)
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output of `netstat -anv -p tcp` on the owner's Mac (macOS 26.5,
    /// Herd running), cut to a few rows. Remote and LAN addresses are
    /// replaced with documentation addresses.
    const NETSTAT: &str = include_str!("../../../fixtures/ports/netstat-macos.txt");
    /// Real output of `ps -p <pid> -o user=,comm=` for Herd's processes. The
    /// owner's user name and home folder are replaced with `dev`.
    const PS_DNSMASQ: &str = include_str!("../../../fixtures/ports/ps-dnsmasq.txt");
    const PS_NGINX_MASTER: &str = include_str!("../../../fixtures/ports/ps-nginx-master.txt");
    const PS_NGINX_WORKER: &str = include_str!("../../../fixtures/ports/ps-nginx-worker.txt");

    fn listener(address: &str, port: u16, process: &str, pid: u32) -> Listener {
        Listener {
            address: address.to_string(),
            port,
            process: Some(process.to_string()),
            pid: Some(pid),
        }
    }

    /// One netstat row in the real layout.
    fn row(proto: &str, local: &str, state: &str, owner: &str) -> String {
        format!(
            "{proto:<6}     0      0  {local:<22} {:<22} {state:<11}            0            0  131072  131072 {owner:>22}  00100 00000006 0000000000ee5bf6 00000000 00000800      1      0 000000",
            "*.*"
        )
    }

    fn listen(proto: &str, local: &str) -> String {
        row(proto, local, "LISTEN", "nginx-arm64:15464")
    }

    #[test]
    fn reads_every_listen_row_of_the_real_output() {
        assert_eq!(
            parse_netstat(NETSTAT),
            vec![
                listener("127.0.0.1", 18080, "caddy", 96172),
                listener("127.0.0.1", 60, "nginx-arm64", 15464),
                listener("127.0.0.1", 443, "nginx-arm64", 15464),
                listener("127.0.0.1", 80, "nginx-arm64", 15464),
                listener("127.0.0.1", 53, "dnsmasq-arm64", 14847),
                listener("::1", 2304, "Herd", 13508),
                listener("127.0.0.1", 58756, "Sample Code IDE", 13143),
                listener("::1", 7679, "Sample Drive", 1313),
                listener("*", 3306, "mysqld", 2836),
                listener("*", 5000, "ControlCenter", 589),
                listener("*", 5000, "ControlCenter", 589),
            ]
        );
    }

    #[test]
    fn herd_holds_80_and_443_in_the_real_output() {
        let listeners = parse_netstat(NETSTAT);
        let nginx = listener("127.0.0.1", 80, "nginx-arm64", 15464);
        assert_eq!(holders(&listeners, 80), vec![&nginx]);

        let nginx = listener("127.0.0.1", 443, "nginx-arm64", 15464);
        assert_eq!(holders(&listeners, 443), vec![&nginx]);

        let dnsmasq = listener("127.0.0.1", 53, "dnsmasq-arm64", 14847);
        assert_eq!(holders(&listeners, 53), vec![&dnsmasq]);
    }

    #[test]
    fn wildcard_rows_are_holders() {
        // Real rows: AirPlay's `*.5000` (tcp6 and tcp4) and a dual-stack `*.3306`.
        let listeners = parse_netstat(NETSTAT);
        let control_center = listener("*", 5000, "ControlCenter", 589);
        assert_eq!(
            holders(&listeners, 5000),
            vec![&control_center, &control_center]
        );
        assert_eq!(holders(&listeners, 3306).len(), 1);

        for proto in ["tcp4", "tcp6", "tcp46"] {
            let listeners = parse_netstat(&listen(proto, "*.80"));
            let holder = listener("*", 80, "nginx-arm64", 15464);
            assert_eq!(holders(&listeners, 80), vec![&holder], "{proto}");
        }
    }

    #[test]
    fn ipv6_loopback_rows_are_holders() {
        let listeners = parse_netstat(&listen("tcp6", "::1.443"));
        let holder = listener("::1", 443, "nginx-arm64", 15464);
        assert_eq!(holders(&listeners, 443), vec![&holder]);

        // A real row: Herd's own `::1.2304`.
        let listeners = parse_netstat(NETSTAT);
        assert_eq!(
            holders(&listeners, 2304)[0].process.as_deref(),
            Some("Herd")
        );
    }

    #[test]
    fn the_port_must_match_exactly() {
        let text = [
            "127.0.0.1.8080",
            "127.0.0.1.180",
            "127.0.0.1.800",
            "127.0.0.1.18080",
            "*.8080",
            "::1.180",
        ]
        .map(|local| listen("tcp4", local))
        .join("\n");
        let listeners = parse_netstat(&text);
        assert_eq!(listeners.len(), 6);
        assert!(holders(&listeners, 80).is_empty());
        assert!(holders(&listeners, 8).is_empty());
        assert_eq!(holders(&listeners, 8080).len(), 2);

        // In the real output, Caddy's `127.0.0.1.18080` holds 18080, not 80.
        let real = parse_netstat(NETSTAT);
        assert_eq!(holders(&real, 18080)[0].process.as_deref(), Some("caddy"));
        assert!(
            holders(&real, 80)
                .iter()
                .all(|holder| holder.pid != Some(96172))
        );
    }

    #[test]
    fn other_addresses_are_not_holders() {
        for local in [
            "192.0.2.5.80",
            "127.0.0.2.80",
            "fe80::1%lo0.80",
            "2001:db8::1.80",
        ] {
            let listeners = parse_netstat(&listen("tcp4", local));
            assert_eq!(listeners.len(), 1, "{local}");
            assert!(holders(&listeners, 80).is_empty(), "{local}");
        }
        for local in ["0.0.0.0.80", "::.80", "::ffff:127.0.0.1.80"] {
            let listeners = parse_netstat(&listen("tcp6", local));
            assert_eq!(holders(&listeners, 80).len(), 1, "{local}");
        }
    }

    #[test]
    fn skips_headers_and_broken_lines() {
        let text = [
            "Active Internet connections (including servers)",
            "Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)",
            "",
            "   ",
            "garbage",
            "tcp4       0      0  127.0.0.1.80",
            "tcp4       0      0  127.0.0.1.80           *.*",
            "tcp4       0      0  127.0.0.1.80           *.*                    LISTE",
            "مرسى tcp4 ٠ ٠ 127.0.0.1.80 *.* LISTEN",
            &listen("udp4", "*.80"),
            &listen("tcp4", "127.0.0.1.http"),
            &listen("tcp4", "127.0.0.1.70000"),
            &listen("tcp4", "127.0.0.1.+80"),
            &listen("tcp4", "127.0.0.1."),
            &listen("tcp4", ".80"),
            &listen("tcp4", "127001"),
            &listen("tcp4", "*.*"),
            &listen("tcp4", "127.0.0.1.80").replacen("0", "x", 1),
            &row("tcp4", "127.0.0.1.80", "ESTABLISHED", "nginx-arm64:15464"),
            &row("tcp4", "127.0.0.1.80", "TIME_WAIT", "nginx-arm64:15464"),
        ]
        .join("\n");
        assert_eq!(parse_netstat(&text), vec![]);
    }

    #[test]
    fn skips_the_rows_of_other_states_in_the_real_output() {
        let not_listen = NETSTAT
            .lines()
            .filter(|line| line.starts_with("tcp") && !line.contains(" LISTEN "))
            .count();
        assert_eq!(
            not_listen, 3,
            "the fixture keeps two ESTABLISHED and one TIME_WAIT row"
        );
        assert_eq!(parse_netstat(NETSTAT).len(), 11);
    }

    #[test]
    fn cuts_carriage_returns() {
        let crlf = NETSTAT.replace('\n', "\r\n");
        assert_eq!(parse_netstat(&crlf), parse_netstat(NETSTAT));

        // A last line that ends in `\r` alone.
        let last = format!("{}\r", listen("tcp4", "127.0.0.1.80"));
        assert_eq!(parse_netstat(&last)[0].port, 80);
    }

    #[test]
    fn a_row_without_a_name_or_a_pid_is_still_a_listener() {
        let cut = "tcp4       0      0  127.0.0.1.80           *.*                    LISTEN                 0            0  131072  131072";
        let listeners = parse_netstat(cut);
        let expected = Listener {
            address: "127.0.0.1".to_string(),
            port: 80,
            process: None,
            pid: None,
        };
        assert_eq!(listeners, vec![expected.clone()]);
        assert_eq!(holders(&listeners, 80), vec![&expected]);

        let no_name = parse_netstat(&row("tcp4", "127.0.0.1.80", "LISTEN", ":15464"));
        assert_eq!(no_name[0].process, None);
        assert_eq!(no_name[0].pid, Some(15464));

        let bad_pid = parse_netstat(&row("tcp4", "127.0.0.1.80", "LISTEN", "nginx:99999999999"));
        assert_eq!(bad_pid[0].process, None);
        assert_eq!(bad_pid[0].pid, None);
    }

    #[test]
    fn the_pid_is_after_the_last_colon() {
        // Names with spaces are in the real output: `Sample Drive:1313`.
        let colon = parse_netstat(&row("tcp4", "127.0.0.1.80", "LISTEN", "a:b:42"));
        assert_eq!(colon[0].process.as_deref(), Some("a:b"));
        assert_eq!(colon[0].pid, Some(42));
    }

    #[test]
    fn parse_ps_reads_the_real_output() {
        assert_eq!(
            parse_ps(PS_DNSMASQ),
            Some(ProcessInfo {
                user: "nobody".to_string(),
                command: "/Applications/Herd.app/Contents/Resources/dnsmasq-arm64".to_string(),
            })
        );
        assert_eq!(
            parse_ps(PS_NGINX_WORKER),
            Some(ProcessInfo {
                user: "dev".to_string(),
                command: "nginx: worker process".to_string(),
            })
        );
        let master = parse_ps(PS_NGINX_MASTER).expect("the master line");
        assert_eq!(master.user, "root");
        assert!(
            master
                .command
                .starts_with("nginx: master process /Applications/Herd.app/")
        );
        assert!(master.command.ends_with("/Herd/config/nginx/nginx.conf"));
    }

    #[test]
    fn parse_ps_reads_one_line_or_nothing() {
        let dnsmasq = Some(ProcessInfo {
            user: "nobody".to_string(),
            command: "/usr/sbin/dnsmasq".to_string(),
        });
        assert_eq!(parse_ps("nobody /usr/sbin/dnsmasq"), dnsmasq);
        assert_eq!(parse_ps("nobody /usr/sbin/dnsmasq\n"), dnsmasq);
        assert_eq!(parse_ps("\n  nobody    /usr/sbin/dnsmasq  \r\n"), dnsmasq);
        assert_eq!(parse_ps("nobody\t/usr/sbin/dnsmasq"), dnsmasq);

        assert_eq!(parse_ps(""), None);
        assert_eq!(parse_ps("\n"), None);
        assert_eq!(parse_ps("  \r\n  "), None);
        assert_eq!(parse_ps("nobody"), None);
        assert_eq!(parse_ps("nobody   \r\n"), None);
    }

    #[test]
    fn known_app_is_a_small_hint() {
        let command = |text| parse_ps(text).expect("a ps line").command;
        let (dnsmasq, master, worker) = (
            command(PS_DNSMASQ),
            command(PS_NGINX_MASTER),
            command(PS_NGINX_WORKER),
        );
        let cases = [
            // The real `ps` commands and netstat names of Herd's processes.
            // The worker's title gives no hint: its netstat name does.
            (dnsmasq.as_str(), Some("Herd")),
            (master.as_str(), Some("Herd")),
            (worker.as_str(), None),
            ("nginx-arm64", Some("Herd")),
            ("dnsmasq-arm64", Some("Herd")),
            ("Herd", Some("Herd")),
            ("/Applications/Herd.app/Contents/MacOS/Herd", Some("Herd")),
            ("herd-devtools-he", Some("Herd")),
            ("/opt/herd/nginx-arm64 -c nginx.conf", Some("Herd")),
            ("com.docker.backend", Some("Docker")),
            ("com.docker.backe", Some("Docker")),
            (
                "/Applications/Docker.app/Contents/MacOS/com.docker.backend",
                Some("Docker"),
            ),
            ("vpnkit-bridge", Some("Docker")),
            (
                "/Applications/MAMP/Library/bin/httpd -k start",
                Some("MAMP"),
            ),
            (
                "/Applications/MAMP PRO.app/Contents/MacOS/MAMP PRO",
                Some("MAMP"),
            ),
            ("/Applications/XAMPP/xamppfiles/bin/httpd", Some("XAMPP")),
            ("/opt/homebrew/opt/nginx/bin/nginx", None),
            ("nginx", None),
            (
                "/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter",
                None,
            ),
            ("shepherd", None),
            ("", None),
        ];
        for (command, app) in cases {
            assert_eq!(known_app(command), app, "{command}");
        }
    }
}
