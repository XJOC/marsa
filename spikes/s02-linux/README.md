# S-02: what the helper needs on Linux

Questions, each answered by a real run on GitHub runners (Ubuntu 22.04 and
24.04) and on Fedora 45 in a container whose PID 1 is systemd:

1. Does systemd-resolved accept a DNS server on a port other than 53?
2. How does Caddy get ports 80 and 443 without root?
3. How does Marsa's CA reach the system trust store and the browsers?
4. Does `pkexec` work with no screen, and what does it pass on?

The decision is ADR 0014 (`docs/decisions/`, kept outside the repository).

## How it runs

The workflow `.github/workflows/spike-s02-linux.yml` runs on a push to a
`task/S-02-*` branch. Each job runs `run-tests.sh` and keeps two files as an
artifact: `summary.txt` (one line per result) and `full.log` (every command,
its output, and its exit code).

**Do not run `run-tests.sh` on your own machine.** It changes the system with
`sudo`: the resolved config, file capabilities, a sysctl, the CA store, and a
polkit rule. It refuses to start unless `CI=true`.

| File | What it is |
| --- | --- |
| `run-tests.sh` | the cases T0 to T10 |
| `dns-standin.py` | a DNS server with the answers of `marsad`: `127.0.0.1` and `::1` under `.test`, `REFUSED` outside it; it logs every query |
| `cap-probe.py` | two ways to set a file capability without following a link |
| `fedora.Dockerfile`, `fedora-prepare.sh` | Fedora with systemd, with its DNS sent through systemd-resolved |

## Results

Runs [42920146480](https://github.com/XJOC/marsa/actions/runs/42920146480)
and [42923205827](https://github.com/XJOC/marsa/actions/runs/42923205827).
The same on all three systems unless a line says otherwise.

| Case | Result |
| --- | --- |
| T2 | A drop-in with `DNS=127.0.0.1:18053` and `Domains=~test` makes `laravel-app.test` resolve (systemd 249, 255, 262). Every other query also reaches our server, which answers `REFUSED`; no lookup failed, with our server up or down. |
| T2b | Next to another global `DNS=` line, `.test` resolves 0 of 6 times. `resolvectl dns` then shows two servers on its `Global:` line. |
| T2c, T2d | Fedora only (needs systemd 258): a DNS delegate file sends no other query to us and works next to a global `DNS=` line (6 of 6). |
| T3 | `setcap cap_net_bind_service=+ep` lets Caddy bind `127.0.0.1:80` and `:443`. A copy of the file, or a write to it, has no capability. |
| T4 | With that capability, the same user cannot read `/proc/<pid>/exe`. `cmdline`, `stat`, and `kill -TERM` still work. |
| T5 | `setcap` refuses `/proc/<pid>/fd/<n>` and a symlink path. `fsetxattr` on an `O_NOFOLLOW` descriptor works. |
| T6 | On a `nosuid` mount the capability is ignored: `bind: permission denied`. |
| T7 | `net.ipv4.ip_unprivileged_port_start=80`: Caddy binds 80 and 443 with no capability, and `/proc/<pid>/exe` stays readable. A file in `/etc/sysctl.d` survives `systemd-sysctl` (a boot). Removal: `rm`, set 1024, `sysctl --system`. |
| T8 | `update-ca-certificates` (Ubuntu) and `update-ca-trust` (Fedora) make `curl https://laravel-app.test` work with the system store, through resolved and port 443. Removal makes it fail again (exit 60). |
| T9 | Chrome ignores the system store. It loads the page after `certutil -A` into `~/.pki/nssdb`, run as the user. `certutil` is not in a default install. |
| T10 | `pkexec` exits 127 with no agent. With a polkit rule for the user it runs the program as root, passes a JSON argument unchanged (also 64 KiB), passes exit codes 1 and 2, and sets `PKEXEC_UID`. Ubuntu 22.04 needs a `.pkla` file, the others a `rules.d` file. |

Chosen: the sysctl for the ports (T4 and T6 rule out `setcap`), the resolved
drop-in for `.test` (the delegate file later, for systemd 258 and up), the
system tool for the CA, `certutil` as the user for browsers, and `pkexec`.

Not tested: the password prompt and its cancel, Firefox, a real Fedora (own
kernel, SELinux, firewalld), and a reboot.
