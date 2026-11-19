//! Spike S-06: does Caddy survive a crash of the daemon, and can the next
//! daemon take it back? Subcommands: `parent` plays `marsad`, `recover` plays
//! the next start, `probe` measures request gaps, `squat` holds a port.

mod caddy_config;
mod parent;
mod probe;
mod recover;
mod sys;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

/// Paths and ports shared by every subcommand.
pub struct Env {
    /// The Caddy binary in `vendor/`. Always an absolute path.
    pub caddy: PathBuf,
    /// All runtime state lives here: config, pid files, logs, Caddy's XDG dirs.
    pub work: PathBuf,
    pub app_port: u16,
    pub admin_port: u16,
}

impl Env {
    fn new(app_port: u16, admin_port: u16) -> Self {
        // The path of this spike folder, fixed at compile time.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        Self {
            caddy: root.join("vendor").join("caddy"),
            // `S06_WORK` lets a script keep several runs apart.
            work: std::env::var_os("S06_WORK")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("work")),
            app_port,
            admin_port,
        }
    }

    pub fn run_dir(&self) -> PathBuf {
        self.work.join("run")
    }

    /// The pid file our parent writes.
    pub fn pidfile(&self) -> PathBuf {
        self.run_dir().join("caddy.pid")
    }

    /// The pid file written by Caddy's own `--pidfile` flag.
    pub fn caddy_pidfile(&self) -> PathBuf {
        self.run_dir().join("caddy-own.pid")
    }

    pub fn config_path(&self) -> PathBuf {
        self.work.join("caddy.json")
    }

    pub fn log_path(&self) -> PathBuf {
        self.work.join("logs").join("caddy.log")
    }

    pub fn admin_url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.admin_port)
    }

    pub fn app_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.app_port)
    }

    pub fn caddy_exe(&self) -> String {
        self.caddy.display().to_string()
    }
}

/// An HTTP client for 127.0.0.1 only: no proxy, no connection reuse.
pub fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .timeout(timeout)
        .build()
        .context("build the HTTP client")
}

#[derive(Parser)]
#[command(name = "s06", about = "Spike S-06: Caddy orphan recovery")]
struct Cli {
    /// Port of the site that Caddy serves.
    #[arg(long, env = "S06_APP_PORT", default_value_t = 18106, global = true)]
    app_port: u16,
    /// Port of Caddy's admin API.
    #[arg(long, env = "S06_ADMIN_PORT", default_value_t = 2106, global = true)]
    admin_port: u16,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Plays marsad: starts Caddy, prints READY, then dies as asked.
    Parent(parent::Args),
    /// Plays the next marsad start: inspect, decide, act.
    Recover(recover::Args),
    /// Sends a request every 25 ms and reports the failure gap.
    Probe(probe::Args),
    /// Holds a TCP port without serving anything.
    Squat {
        #[arg(long)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let env = Env::new(cli.app_port, cli.admin_port);
    match cli.command {
        Command::Parent(args) => parent::run(&env, args).await,
        Command::Recover(args) => recover::run(&env, args).await,
        Command::Probe(args) => probe::run(&env, args).await,
        Command::Squat { port } => squat(port).await,
    }
}

async fn squat(port: u16) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("bind 127.0.0.1:{port}"))?;
    println!("SQUAT_READY pid={} port={port}", std::process::id());
    // Never accept: the port stays held until this process is killed.
    let _keep = listener;
    std::future::pending::<()>().await;
    Ok(())
}
