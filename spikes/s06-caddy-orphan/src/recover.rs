//! `s06 recover`: plays the next `marsad` start. It inspects the machine with
//! three methods (pid file, admin API, port), decides what to do with a pure
//! function, and then acts: `reap` or `adopt`.
//!
//! The pure parts (`PidfileState`, `AdminState`, `PortState`, `decide`) are
//! written the way the product would put them in `marsa-core`. The probing
//! and the killing would live in `marsad`.

use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args as ClapArgs, ValueEnum};

use crate::parent::{self, SpawnOpts};
use crate::sys::{self, PidRecord};
use crate::{Env, caddy_config, http_client};

// ---------------------------------------------------------------------------
// Pure part: states and the decision
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PidfileState {
    /// No readable file.
    Missing,
    /// The file names a process that is gone.
    Stale { pid: u32 },
    /// The pid is alive but it is not the process we started.
    Mismatch { pid: u32, found: String },
    /// Alive, same executable path, same start time.
    Ours { pid: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminState {
    /// `GET /id/marsa-srv` answered 200 with our id.
    Ours,
    /// Something answered, but without our id.
    Foreign,
    /// Connection refused: nothing listens.
    NoReply,
    /// Connected, but no answer in time (a frozen process).
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortState {
    Free,
    /// Someone listens. The owner is `name(pid)` from `lsof`.
    Busy {
        owner: String,
    },
}

#[derive(Debug, Clone)]
pub struct Inspection {
    pub pidfile: PidfileState,
    pub admin: AdminState,
    pub app_port: PortState,
    pub admin_port: PortState,
    /// The pid that `lsof` finds on the admin port, only when its executable
    /// path is our Caddy. A second source of a verified pid, besides the pid
    /// file.
    pub admin_owner: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Strategy {
    Reap,
    Adopt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing of ours is running. Start Caddy. Remove a dead pid file first.
    StartFresh { remove_pidfile: bool },
    /// Stop the orphan (gently first), then start a new Caddy.
    Reap { pid: Option<u32> },
    /// Keep the orphan and send it a new config.
    Adopt { pid: Option<u32> },
    /// Someone else holds our ports. Report it and touch nothing.
    Conflict { reason: String },
}

impl fmt::Display for PidfileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "missing"),
            Self::Stale { pid } => write!(f, "stale(pid={pid})"),
            Self::Mismatch { pid, found } => write!(f, "mismatch(pid={pid},found={found})"),
            Self::Ours { pid } => write!(f, "ours(pid={pid})"),
        }
    }
}

impl fmt::Display for AdminState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Ours => "ours",
            Self::Foreign => "foreign",
            Self::NoReply => "no-reply",
            Self::Timeout => "timeout",
        };
        write!(f, "{text}")
    }
}

impl fmt::Display for PortState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Free => write!(f, "free"),
            Self::Busy { owner } => write!(f, "busy({owner})"),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartFresh { remove_pidfile } => {
                write!(f, "start-fresh(remove_pidfile={remove_pidfile})")
            }
            Self::Reap { pid } => write!(f, "reap(pid={pid:?})"),
            Self::Adopt { pid } => write!(f, "adopt(pid={pid:?})"),
            Self::Conflict { reason } => write!(f, "conflict({reason})"),
        }
    }
}

/// Turns inspection results into one action. Identity comes from the admin
/// API (our `@id`). The pid file only supplies a pid to signal, and only after
/// its name and start time matched. A port alone never authorizes a kill.
pub fn decide(i: &Inspection, strategy: Strategy) -> Action {
    let from_file = match i.pidfile {
        PidfileState::Ours { pid } => Some(pid),
        _ => None,
    };
    // A pid we may signal: from the pid file, or else the verified owner of
    // our admin port.
    let ours_pid = from_file.or(i.admin_owner);
    match i.admin {
        AdminState::Foreign => Action::Conflict {
            reason: "a Caddy that is not ours answers on our admin port".to_string(),
        },
        AdminState::Ours => match strategy {
            Strategy::Reap => Action::Reap { pid: ours_pid },
            Strategy::Adopt => Action::Adopt { pid: ours_pid },
        },
        // A frozen process cannot be adopted, only removed, and removing it
        // needs a verified pid.
        AdminState::Timeout => match ours_pid {
            Some(pid) => Action::Reap { pid: Some(pid) },
            None => Action::Conflict {
                reason: "a process holds our admin port, does not answer, and cannot be verified as our Caddy"
                    .to_string(),
            },
        },
        AdminState::NoReply => {
            if let Some(pid) = from_file {
                return Action::Reap { pid: Some(pid) };
            }
            for (name, state) in [("site", &i.app_port), ("admin", &i.admin_port)] {
                if let PortState::Busy { owner } = state {
                    return Action::Conflict {
                        reason: format!("the {name} port is held by {owner}, which is not our Caddy"),
                    };
                }
            }
            Action::StartFresh {
                remove_pidfile: matches!(
                    i.pidfile,
                    PidfileState::Stale { .. } | PidfileState::Mismatch { .. }
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Probing (impure)
// ---------------------------------------------------------------------------

async fn inspect_pidfile(env: &Env, name_only: bool) -> PidfileState {
    let Ok(text) = tokio::fs::read_to_string(env.pidfile()).await else {
        return PidfileState::Missing;
    };
    let Ok(record) = serde_json::from_str::<PidRecord>(&text) else {
        return PidfileState::Missing;
    };
    let pid = record.pid;
    if !sys::is_alive(pid).await {
        return PidfileState::Stale { pid };
    }
    let found = sys::exe_of(pid).await.unwrap_or_default();
    if found != record.exe || found != env.caddy_exe() {
        return PidfileState::Mismatch { pid, found };
    }
    if !name_only {
        let started = sys::started_of(pid).await.unwrap_or_default();
        if started != record.started {
            return PidfileState::Mismatch {
                pid,
                found: format!(
                    "{found} (start time {started} differs from {})",
                    record.started
                ),
            };
        }
    }
    PidfileState::Ours { pid }
}

async fn inspect_admin(env: &Env) -> AdminState {
    let Ok(client) = http_client(Duration::from_millis(1000)) else {
        return AdminState::NoReply;
    };
    let url = env.admin_url(&format!("/id/{}", caddy_config::SERVER_ID));
    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let text = resp.text().await.unwrap_or_default();
            let tagged = match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => v["@id"] == caddy_config::SERVER_ID,
                Err(_) => false,
            };
            if tagged {
                AdminState::Ours
            } else {
                AdminState::Foreign
            }
        }
        Ok(_) => AdminState::Foreign,
        Err(e) if e.is_timeout() => AdminState::Timeout,
        Err(_) => AdminState::NoReply,
    }
}

async fn inspect_port(port: u16) -> PortState {
    if sys::port_is_free(port) {
        return PortState::Free;
    }
    let owner = match sys::listener_owner(port).await {
        Some((pid, name)) => format!("{name}({pid})"),
        None => "unknown".to_string(),
    };
    PortState::Busy { owner }
}

struct Timings {
    pidfile: Duration,
    admin: Duration,
    port: Duration,
}

async fn inspect(env: &Env, name_only: bool, ignore_pidfile: bool) -> (Inspection, Timings) {
    let t = Instant::now();
    let pidfile = if ignore_pidfile {
        PidfileState::Missing
    } else {
        inspect_pidfile(env, name_only).await
    };
    let pidfile_t = t.elapsed();
    let t = Instant::now();
    let admin = inspect_admin(env).await;
    let admin_t = t.elapsed();
    let t = Instant::now();
    let app_port = inspect_port(env.app_port).await;
    let admin_port = inspect_port(env.admin_port).await;
    // Only an answering or a frozen admin API can belong to us, so only then
    // is the owner worth a second look.
    let mut admin_owner = None;
    if matches!(admin, AdminState::Ours | AdminState::Timeout)
        && let Some((pid, _)) = sys::listener_owner(env.admin_port).await
        && sys::exe_of(pid).await.as_deref() == Some(env.caddy_exe().as_str())
    {
        admin_owner = Some(pid);
    }
    let port_t = t.elapsed();
    (
        Inspection {
            pidfile,
            admin,
            app_port,
            admin_port,
            admin_owner,
        },
        Timings {
            pidfile: pidfile_t,
            admin: admin_t,
            port: port_t,
        },
    )
}

// ---------------------------------------------------------------------------
// Acting (impure)
// ---------------------------------------------------------------------------

fn step(t0: &Instant, label: &str, detail: &str) {
    println!("STEP t_ms={} {label} {detail}", t0.elapsed().as_millis());
}

/// Signals a pid only if it still runs our Caddy binary. Checked again right
/// before each signal, because the pid may have been reused since the
/// inspection.
async fn signal_if_ours(env: &Env, pid: u32, sig: &str) -> Result<bool> {
    let found = sys::exe_of(pid).await;
    if found.as_deref() != Some(env.caddy_exe().as_str()) {
        println!("REFUSED signal={sig} pid={pid} found={found:?}");
        return Ok(false);
    }
    sys::signal(pid, sig).await
}

/// The escalation ladder: `POST /stop`, then SIGTERM after 2 s, then SIGKILL
/// 5 s later. Returns once both ports are free.
async fn stop_orphan(env: &Env, pid: Option<u32>, t0: &Instant) -> Result<()> {
    let ports = [env.app_port, env.admin_port];
    let client = http_client(Duration::from_secs(2))?;
    let stop_started = Instant::now();
    let outcome = match client.post(env.admin_url("/stop")).send().await {
        Ok(resp) => format!("http={}", resp.status().as_u16()),
        Err(e) if e.is_timeout() => "timeout".to_string(),
        Err(e) => format!("error={e}"),
    };
    step(t0, "stop_api", &outcome);
    let left = Duration::from_secs(2).saturating_sub(stop_started.elapsed());
    if sys::wait_ports_free(&ports, left).await {
        step(t0, "ports_free", "after=stop_api");
        return Ok(());
    }

    let Some(pid) = pid else {
        bail!("the orphan did not stop and there is no verified pid to escalate with");
    };
    let sent = signal_if_ours(env, pid, "TERM").await?;
    step(t0, "sigterm", &format!("pid={pid} sent={sent}"));
    if sys::wait_ports_free(&ports, Duration::from_secs(5)).await {
        step(t0, "ports_free", "after=sigterm");
        return Ok(());
    }

    let sent = signal_if_ours(env, pid, "KILL").await?;
    step(t0, "sigkill", &format!("pid={pid} sent={sent}"));
    if sys::wait_ports_free(&ports, Duration::from_secs(2)).await {
        step(t0, "ports_free", "after=sigkill");
        return Ok(());
    }
    bail!("the ports are still busy after SIGKILL")
}

/// Starts a Caddy that this process owns, waits for it, holds it for a while,
/// and stops it cleanly.
async fn start_and_hold(env: &Env, body: &str, hold: Duration, t0: &Instant) -> Result<()> {
    let mut sp = parent::spawn_caddy(env, SpawnOpts::recommended(), body).await?;
    parent::wait_ready(env, Duration::from_secs(10)).await?;
    step(t0, "new_caddy_ready", &format!("pid={}", sp.pid));
    let served = http_client(Duration::from_secs(2))?
        .get(env.app_url())
        .send()
        .await?
        .text()
        .await?;
    step(t0, "serving", &format!("body={served:?}"));
    println!("READY parent={} caddy={}", std::process::id(), sp.pid);
    tokio::time::sleep(hold).await;

    // Clean shutdown: ask over the admin API, then wait for the child.
    let client = http_client(Duration::from_secs(2))?;
    let _ = client.post(env.admin_url("/stop")).send().await;
    match tokio::time::timeout(Duration::from_secs(5), sp.child.wait()).await {
        Ok(status) => println!("CLEAN_STOP status={status:?}"),
        Err(_) => {
            sp.child.start_kill().context("kill the new Caddy")?;
            println!("CLEAN_STOP forced");
        }
    }
    Ok(())
}

/// Adopt: send the orphan a new config over `/load` and watch it by pid.
async fn adopt(
    env: &Env,
    pid: Option<u32>,
    body: &str,
    hold: Duration,
    t0: &Instant,
) -> Result<()> {
    let config = caddy_config::build(env.app_port, env.admin_port, body, true);
    let client = http_client(Duration::from_secs(2))?;
    let resp = client
        .post(env.admin_url("/load"))
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&config)?)
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => step(t0, "load", "http=200"),
        Ok(r) => bail!("ADOPT_FAILED /load answered {}", r.status()),
        Err(e) if e.is_timeout() => bail!("ADOPT_FAILED /load timed out"),
        Err(e) => bail!("ADOPT_FAILED /load error: {e}"),
    }
    let served = http_client(Duration::from_secs(2))?
        .get(env.app_url())
        .send()
        .await?
        .text()
        .await?;
    step(t0, "serving", &format!("body={served:?}"));
    println!("ADOPTED pid={pid:?}");

    // No `Child` to wait on: the only way to watch the process is by pid.
    let end = Instant::now() + hold;
    while Instant::now() < end {
        if let Some(p) = pid {
            println!("WATCH pid={p} alive={}", sys::is_alive(p).await);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let _ = client.post(env.admin_url("/stop")).send().await;
    Ok(())
}

#[derive(ClapArgs)]
pub struct Args {
    /// `detect` only prints. `reap` and `adopt` also act.
    #[arg(long, value_enum, default_value_t = Mode::Detect)]
    pub action: Mode,
    /// The text that the site answers with after recovery.
    #[arg(long, default_value = "marsa-spike v2")]
    pub body: String,
    /// How long to own the new Caddy before stopping it.
    #[arg(long, default_value_t = 3)]
    pub hold_secs: u64,
    /// Compare only the executable path in the pid file check.
    #[arg(long)]
    pub name_only: bool,
    /// Pretend there is no pid file at all.
    #[arg(long)]
    pub ignore_pidfile: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Detect,
    Reap,
    Adopt,
}

pub async fn run(env: &Env, args: Args) -> Result<()> {
    let t0 = Instant::now();
    let strategy = if args.action == Mode::Adopt {
        Strategy::Adopt
    } else {
        Strategy::Reap
    };
    let (inspection, timings) = inspect(env, args.name_only, args.ignore_pidfile).await;
    println!(
        "INSPECT pidfile={} admin={} app_port={} admin_port={}",
        inspection.pidfile, inspection.admin, inspection.app_port, inspection.admin_port
    );
    println!("ADMIN_OWNER {:?}", inspection.admin_owner);
    println!(
        "TIMING pidfile_ms={:.1} admin_ms={:.1} port_ms={:.1}",
        timings.pidfile.as_secs_f64() * 1e3,
        timings.admin.as_secs_f64() * 1e3,
        timings.port.as_secs_f64() * 1e3
    );
    let action = decide(&inspection, strategy);
    println!("DECISION {action}");
    step(&t0, "decided", "");
    if args.action == Mode::Detect {
        return Ok(());
    }

    let hold = Duration::from_secs(args.hold_secs);
    match action {
        Action::Conflict { reason } => {
            println!("CONFLICT {reason}");
            std::process::exit(3);
        }
        Action::StartFresh { remove_pidfile } => {
            if remove_pidfile {
                let _ = tokio::fs::remove_file(env.pidfile()).await;
                step(&t0, "removed_stale_pidfile", "");
            }
            start_and_hold(env, &args.body, hold, &t0).await
        }
        Action::Reap { pid } => {
            stop_orphan(env, pid, &t0).await?;
            start_and_hold(env, &args.body, hold, &t0).await
        }
        Action::Adopt { pid } => adopt(env, pid, &args.body, hold, &t0).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy(owner: &str) -> PortState {
        PortState::Busy {
            owner: owner.to_string(),
        }
    }

    fn inspection(
        pidfile: PidfileState,
        admin: AdminState,
        app: PortState,
        adm: PortState,
    ) -> Inspection {
        Inspection {
            pidfile,
            admin,
            app_port: app,
            admin_port: adm,
            admin_owner: None,
        }
    }

    #[test]
    fn r1_orphan_with_pid_file_is_reaped_by_pid() {
        let i = inspection(
            PidfileState::Ours { pid: 7 },
            AdminState::Ours,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        assert_eq!(decide(&i, Strategy::Reap), Action::Reap { pid: Some(7) });
        assert_eq!(decide(&i, Strategy::Adopt), Action::Adopt { pid: Some(7) });
    }

    #[test]
    fn r2_orphan_without_pid_file_is_still_ours() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::Ours,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        assert_eq!(decide(&i, Strategy::Reap), Action::Reap { pid: None });
    }

    #[test]
    fn r3_stale_pid_file_is_removed_and_caddy_starts() {
        let i = inspection(
            PidfileState::Stale { pid: 7 },
            AdminState::NoReply,
            PortState::Free,
            PortState::Free,
        );
        assert_eq!(
            decide(&i, Strategy::Reap),
            Action::StartFresh {
                remove_pidfile: true
            }
        );
    }

    #[test]
    fn r4_reused_pid_is_never_signalled() {
        let i = inspection(
            PidfileState::Mismatch {
                pid: 7,
                found: "/bin/sleep".to_string(),
            },
            AdminState::NoReply,
            PortState::Free,
            PortState::Free,
        );
        let action = decide(&i, Strategy::Reap);
        assert_eq!(
            action,
            Action::StartFresh {
                remove_pidfile: true
            }
        );
    }

    #[test]
    fn r5_foreign_caddy_is_a_conflict() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::Foreign,
            busy("caddy(9)"),
            busy("caddy(9)"),
        );
        assert!(matches!(
            decide(&i, Strategy::Reap),
            Action::Conflict { .. }
        ));
        assert!(matches!(
            decide(&i, Strategy::Adopt),
            Action::Conflict { .. }
        ));
    }

    #[test]
    fn r5b_foreign_caddy_wins_over_a_matching_pid_file() {
        let i = inspection(
            PidfileState::Ours { pid: 9 },
            AdminState::Foreign,
            busy("caddy(9)"),
            busy("caddy(9)"),
        );
        assert!(matches!(
            decide(&i, Strategy::Reap),
            Action::Conflict { .. }
        ));
    }

    #[test]
    fn r6_another_program_on_the_site_port_is_a_conflict() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::NoReply,
            busy("s06(5)"),
            PortState::Free,
        );
        match decide(&i, Strategy::Reap) {
            Action::Conflict { reason } => assert!(reason.contains("s06(5)")),
            other => panic!("expected a conflict, got {other}"),
        }
    }

    #[test]
    fn r7_frozen_orphan_is_reaped_never_adopted() {
        let i = inspection(
            PidfileState::Ours { pid: 7 },
            AdminState::Timeout,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        assert_eq!(decide(&i, Strategy::Adopt), Action::Reap { pid: Some(7) });
    }

    #[test]
    fn frozen_orphan_without_pid_file_is_reaped_when_the_admin_port_owner_is_verified() {
        let mut i = inspection(
            PidfileState::Missing,
            AdminState::Timeout,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        i.admin_owner = Some(7);
        assert_eq!(decide(&i, Strategy::Reap), Action::Reap { pid: Some(7) });
    }

    #[test]
    fn frozen_process_without_a_verified_pid_is_a_conflict() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::Timeout,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        assert!(matches!(
            decide(&i, Strategy::Reap),
            Action::Conflict { .. }
        ));
    }

    #[test]
    fn nothing_running_starts_fresh_without_touching_files() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::NoReply,
            PortState::Free,
            PortState::Free,
        );
        assert_eq!(
            decide(&i, Strategy::Reap),
            Action::StartFresh {
                remove_pidfile: false
            }
        );
    }
}
