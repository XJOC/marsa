//! Process helpers built on the macOS command line tools, so the spike needs
//! no `unsafe` and no `libc`/`nix`. Every program is an absolute path and gets
//! its arguments as an array, never through a shell (AGENTS.md section 6).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const KILL: &str = "/bin/kill";
const PS: &str = "/bin/ps";
const LSOF: &str = "/usr/sbin/lsof";

async fn tool(program: &str, args: &[&str]) -> Result<std::process::Output> {
    Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("could not run {program}"))
}

/// Sends a signal such as `TERM`, `KILL`, `STOP`, `CONT`, or `0`. Returns
/// whether the OS accepted it. This function does not check who the process
/// is: callers must verify identity first (see `recover::signal_if_ours`).
pub async fn signal(pid: u32, sig: &str) -> Result<bool> {
    let out = tool(KILL, &["-s", sig, &pid.to_string()]).await?;
    Ok(out.status.success())
}

/// One `ps -o <field>=` column for one pid. `None` when the process is gone.
pub async fn proc_field(pid: u32, field: &str) -> Option<String> {
    let out = tool(PS, &["-p", &pid.to_string(), "-o", &format!("{field}=")])
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Alive means it exists and is not a zombie.
pub async fn is_alive(pid: u32) -> bool {
    match proc_field(pid, "stat").await {
        Some(stat) => !stat.starts_with('Z'),
        None => false,
    }
}

/// The path the process was started with (`ps -o comm=` on macOS).
pub async fn exe_of(pid: u32) -> Option<String> {
    proc_field(pid, "comm").await
}

/// The start time as printed by `ps`, with one second of resolution.
pub async fn started_of(pid: u32) -> Option<String> {
    proc_field(pid, "lstart").await
}

/// Who listens on a TCP port: `(pid, command name)`.
pub async fn listener_owner(port: u16) -> Option<(u32, String)> {
    let out = tool(
        LSOF,
        &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"],
    )
    .await
    .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut pid = None;
    let mut name = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix('c') {
            name = Some(rest.to_string());
        }
    }
    Some((pid?, name.unwrap_or_default()))
}

/// Whether nothing listens on `127.0.0.1:port`, by trying to bind it.
pub fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Polls until every port is free or the time is up.
pub async fn wait_ports_free(ports: &[u16], limit: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if ports.iter().all(|p| port_is_free(*p)) {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// What the parent writes right after `spawn`: the pid, the executable path it
/// started, and the start time that `ps` reports for that pid.
#[derive(Debug, Serialize, Deserialize)]
pub struct PidRecord {
    pub pid: u32,
    pub exe: String,
    pub started: String,
}

/// Writes through a temporary file in the same folder, then renames.
pub async fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, content)
        .await
        .with_context(|| format!("write {}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}
