//! The facts of `marsa doctor`, gathered once for the CLI and for
//! `GET /v1/doctor`, so both show the same checks (architecture.md section 6).
//!
//! [`run`] asks the system (the data folder, `netstat`, `ps`, `security`, the
//! resolver, local sockets) and gives each fact to the pure functions of
//! [`crate::doctor`], which return the verdicts. What the daemon says of
//! itself comes in as [`DaemonFacts`]: the CLI asks a running daemon for
//! it, the daemon reads its own state. Read-only: no admin rights, no file
//! written, no program started that changes anything.
//!
//! Every function blocks (programs, sockets, the resolver), so the daemon
//! calls [`run`] on a blocking thread.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::caddy::CaddyPaths;
use crate::config::{self, ConfigPaths};
use crate::data_dir::DataDir;
use crate::databases::ServerKind;
use crate::doctor::{self, CaddyState, Check, Installed, LogTail, Ports, Skip, Verdict, Warning};
use crate::platform::browser::windows_dir;
use crate::platform::runner::RealRunner;
use crate::platform::{linux_dns, probe, trust};
use crate::recipes::{Os, Platform};
use crate::runtimes::{Runtime, RuntimesDir};
use crate::types::{Health, ServiceState};

/// How long the system resolver may take for the `.test` name.
const DNS_LIMIT: Duration = Duration::from_secs(5);
/// How long a connection to port 80 or 443, and the answer on 80, may take.
const PORT_LIMIT: Duration = Duration::from_secs(2);
/// How much of the end of `logs/marsad.log` is read.
const LOG_TAIL: u64 = 64 * 1024;

/// What the daemon says of itself; `health` is `Err` with the error code when
/// it cannot be asked.
#[derive(Debug, Clone)]
pub struct DaemonFacts {
    /// Its health and port, or the code of the client error.
    pub health: Result<(Health, u16), String>,
    pub caddy: Option<ServiceState>,
    /// Caddy's PID, while it runs.
    pub caddy_pid: Option<u32>,
    /// Whether any site is secure. `None` if not known.
    pub secure_site: Option<bool>,
    /// The host names of the sites. `None` if not known.
    pub site_names: Option<Vec<String>>,
}

/// One thing [`run`] found, in the order to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Line(Check, Verdict),
    Warning(Warning),
}

fn line(out: &mut dyn FnMut(Item), check: Check, verdict: Verdict) {
    out(Item::Line(check, verdict));
}

/// Runs the checks in order, and gives each result to `out` as soon as it is
/// known. The `PATH` check judges the terminal's `PATH`, which only a CLI has:
/// `path` is its verdict, or `None` to leave the check out (the daemon).
pub fn run(
    data_dir: &Path,
    daemon: &DaemonFacts,
    path: Option<Verdict>,
    out: &mut dyn FnMut(Item),
) {
    let platform = Platform::current();
    let macos = matches!(platform, Some(Platform::DarwinArm64 | Platform::DarwinX64));
    let linux = matches!(platform, Some(Platform::LinuxX64 | Platform::LinuxArm64));
    let windows = platform == Some(Platform::WindowsX64);
    let running = daemon.health.is_ok();
    line(out, Check::Daemon, doctor::daemon(daemon.health.clone()));
    // Windows: an elevated daemon runs, but PostgreSQL refuses to.
    if let Ok((health, _)) = &daemon.health {
        let elevated = super::process_is_elevated(health.pid);
        if let Some(warning) = doctor::daemon_elevation(elevated) {
            out(Item::Warning(warning));
        }
    }
    let installed = installed(data_dir);
    let caddy_installed = installed
        .caddy
        .as_ref()
        .is_ok_and(|found| !found.is_empty());
    line(out, Check::Runtimes, doctor::runtimes(&installed));
    // The official PHP for Windows needs Microsoft's runtime (ADR 0019).
    // MySQL comes as `tar.xz`, which Linux unpacks with the system's xz.
    if linux {
        line(
            out,
            Check::Xz,
            doctor::xz(crate::installer::extract::xz_available()),
        );
    }
    if windows {
        let servers = vc_servers(data_dir);
        let verdict = doctor::vc_runtime(vc_runtime_version(), servers);
        line(out, Check::VcRuntime, verdict);
        // PHP trusts the bundle marsad builds from Windows' store.
        line(
            out,
            Check::SystemRoots,
            doctor::system_roots(roots_facts(data_dir)),
        );
    }
    let caddy = daemon.caddy.as_ref();
    line(
        out,
        Check::Caddy,
        doctor::caddy(running, caddy, caddy_installed),
    );
    // The checks of the redirect, the resolver file, and the user's trust
    // are macOS ones (ADR 0008, ADR 0015). Linux and Windows set these up
    // in other ways, each with checks of its own.
    if macos {
        let name = doctor::probe_name(probe::random());
        line(out, Check::Dns, dns(&name, running));
        let caddy_running = running.then(|| caddy == Some(&ServiceState::Running));
        let facts = ports(&name, caddy_running, daemon.secure_site);
        line(out, Check::Ports, doctor::ports(facts));
        line(out, Check::Trust, trust(data_dir));
    } else if linux {
        let names = daemon.site_names.as_deref();
        line(out, Check::Dns, linux_names(data_dir, running, names));
        for check in [Check::Ports, Check::Trust] {
            line(out, check, Verdict::Skip(Skip::NotOnThisSystem));
        }
    } else if windows {
        let names = daemon.site_names.as_deref();
        let block = linux_dns::read_block(
            &windows_dir(std::env::var_os("SystemRoot"))
                .join("System32")
                .join(r"drivers\etc\hosts"),
        );
        line(out, Check::Dns, doctor::hosts(block.as_ref(), names));
        let caddy_running = caddy == Some(&ServiceState::Running);
        let caddy = match (running, daemon.caddy_pid.filter(|_| caddy_running)) {
            (false, _) => CaddyState::DaemonDown,
            (true, Some(pid)) => CaddyState::Running(pid),
            (true, None) => CaddyState::Stopped,
        };
        line(
            out,
            Check::Ports,
            doctor::windows_ports(
                super::ports::holders(&RealRunner, Os::Windows, &CancellationToken::new()),
                caddy,
            ),
        );
        line(out, Check::Trust, windows_trust(data_dir));
    } else {
        for check in [Check::Dns, Check::Ports, Check::Trust] {
            line(out, check, Verdict::Skip(Skip::NotOnThisSystem));
        }
    }
    if let Some(path) = path {
        line(out, Check::Path, path);
    }
    line(out, Check::Log, log(data_dir));
}

/// What `runtimes/` and `global.toml` hold, read like the shim reads them.
fn installed(data_dir: &Path) -> Installed {
    let runtimes = RuntimesDir::in_dir(data_dir);
    let list = |runtime| {
        runtimes
            .installed(runtime)
            .map_err(|err| err.code().to_owned())
    };
    let global = config::load_global(&ConfigPaths::in_dir(data_dir).global);
    Installed {
        php: list(Runtime::Php),
        caddy: list(Runtime::Caddy),
        default_php: global
            .map(|global| global.php.default)
            .map_err(|err| err.code().to_owned()),
    }
}

/// Linux: the hosts file where this data folder's setup wrote Marsa's
/// block, else a random name through the `marsa0` link. Reads only: the
/// marker, and `/etc/hosts`, which every user may read.
fn linux_names(data_dir: &Path, running: bool, sites: Option<&[String]>) -> Verdict {
    if linux_dns::marker(data_dir).is_file() {
        let block = linux_dns::read_block(Path::new(linux_dns::HOSTS_FILE));
        return doctor::hosts(block.as_ref(), sites);
    }
    let name = doctor::probe_name(probe::random());
    let answer = probe::resolve(&name, DNS_LIMIT)
        .map_err(|err| tracing::debug!(name, error = %err, "the .test name did not resolve"))
        .ok();
    doctor::link(running, answer.as_deref())
}

fn dns(name: &str, running: bool) -> Verdict {
    let answer = probe::resolve(name, DNS_LIMIT)
        .map_err(|err| tracing::debug!(name, error = %err, "the .test name did not resolve"))
        .ok();
    doctor::dns(running, answer.as_deref())
}

/// The holders of 80 and 443 from `netstat` and `ps`, as `marsa setup`
/// finds them. The ports are tried only when nobody holds them and Caddy
/// runs: the verdict needs them only then, and another program's server is
/// left alone.
fn ports(name: &str, caddy_running: Option<bool>, secure_site: Option<bool>) -> Ports {
    let holders = super::ports::holders(&RealRunner, Os::Mac, &CancellationToken::new());
    let clear = holders.as_ref().is_some_and(Vec::is_empty) && caddy_running == Some(true);
    let http = clear
        && probe::http_get(80, name, PORT_LIMIT)
            .is_ok_and(|answer| doctor::is_marsa_answer(&answer));
    let https = (clear && secure_site == Some(true)).then(|| probe::accepts(443, PORT_LIMIT));
    Ports {
        holders,
        caddy_running,
        http,
        https,
    }
}

/// `security verify-cert` on Marsa's CA root, as `marsa setup` asks it.
fn trust(data_dir: &Path) -> Verdict {
    // Absolute, so `security` reads this file whatever the folder it runs in.
    let Ok(ca_root) = std::path::absolute(CaddyPaths::in_dir(data_dir).ca_root()) else {
        return Verdict::Skip(Skip::NotChecked);
    };
    let exists = ca_root.is_file();
    let trusted = exists
        .then(|| trust::trusted(&RealRunner, &ca_root, &CancellationToken::new()))
        .flatten();
    doctor::trust(exists, trusted)
}

/// `PATH` as this terminal has it, against the shims of `data_dir`.
pub fn path(data_dir: &Path) -> Verdict {
    let shims = shims_dir(data_dir);
    let entries: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    doctor::path(&entries, &shims, |dir| probe::program_in(dir, "php"))
}

fn log(data_dir: &Path) -> Verdict {
    let tail = probe::read_tail(&DataDir::new(data_dir).marsad_log(), LOG_TAIL)
        .map_err(|err| tracing::debug!(error = %err, "cannot read the daemon log"));
    doctor::log(match &tail {
        Ok(Some(text)) => LogTail::Text(text),
        Ok(None) => LogTail::Missing,
        Err(()) => LogTail::Unreadable,
    })
}

/// The folder of the shims, absolute: `PATH` holds absolute folders.
fn shims_dir(data_dir: &Path) -> PathBuf {
    let shims = DataDir::new(data_dir).shims();
    std::path::absolute(&shims).unwrap_or(shims)
}

fn roots_facts(data_dir: &Path) -> doctor::RootsFacts {
    let path = CaddyPaths::in_dir(data_dir).ca_bundle();
    let bundle = std::fs::read_to_string(&path).ok().map(|text| {
        let age = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|written| written.elapsed().ok())
            .map_or(0, |age| age.as_secs() / 86_400);
        (text, age)
    });
    let marsa_root = std::fs::read_to_string(CaddyPaths::in_dir(data_dir).ca_root()).ok();
    doctor::RootsFacts {
        windows: windows_roots(),
        bundle,
        marsa_root,
    }
}

/// The roots Windows trusts for HTTPS, and how many others the store holds;
/// never asked for elsewhere.
fn windows_roots() -> Result<(Vec<Vec<u8>>, usize), String> {
    // `#[cfg]` outside `platform`: the certificate store is Windows'.
    #[cfg(windows)]
    return super::windows::root_certificates()
        .map(|roots| (roots.trusted, roots.left_out))
        .map_err(|err| err.to_string());
    #[cfg(not(windows))]
    Err("not Windows".to_owned())
}

/// Windows: whether Marsa's CA root is among the roots Windows trusts, read
/// as for PHP's bundle (setup puts it in `LocalMachine\Root`).
fn windows_trust(data_dir: &Path) -> Verdict {
    let root = std::fs::read_to_string(CaddyPaths::in_dir(data_dir).ca_root()).ok();
    let want = root.as_deref().and_then(crate::helper::certificate_sha256);
    let trusted = windows_roots().ok().map(|(ders, _)| {
        want.as_ref().is_some_and(|want| {
            ders.iter().any(|der| {
                let pem = crate::caddy::pem_certificates(std::slice::from_ref(der));
                crate::helper::certificate_sha256(&pem).as_ref() == Some(want)
            })
        })
    });
    doctor::trust(root.is_some(), trusted)
}

/// The installed database servers that need the Visual C++ runtime as PHP
/// does: Oracle's MySQL and theseus' PostgreSQL are MSVC builds that bring
/// none (S-12). Valkey is a Cygwin build and needs none.
fn vc_servers(data_dir: &Path) -> Vec<String> {
    let runtimes = RuntimesDir::in_dir(data_dir);
    [ServerKind::Mysql, ServerKind::Postgres]
        .into_iter()
        .filter(|kind| {
            let versions = runtimes.installed(kind.runtime());
            versions.is_ok_and(|versions| !versions.is_empty())
        })
        .map(|kind| kind.name().to_owned())
        .collect()
}

/// The Visual C++ runtime that Windows reports; none elsewhere.
fn vc_runtime_version() -> Option<(u32, u32, u32)> {
    // `#[cfg]` outside `platform`: the registry is Windows'.
    #[cfg(windows)]
    return super::windows::vc_runtime_version();
    #[cfg(not(windows))]
    None
}
