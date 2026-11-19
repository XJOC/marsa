//! `s06 parent`: plays `marsad`. It starts Caddy as a child that a tokio task
//! owns, prints `READY`, and then dies in the way the experiment asks.

use std::collections::VecDeque;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args as ClapArgs, ValueEnum};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio_util::sync::CancellationToken;

use crate::sys::{self, PidRecord};
use crate::{Env, caddy_config, http_client};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Group {
    /// Caddy stays in the parent's process group.
    Inherit,
    /// Caddy gets its own group (`process_group(0)`).
    Own,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StdioMode {
    /// stdout and stderr go to a file.
    File,
    /// stdout and stderr go to a pipe that the parent reads line by line.
    Pipe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PidfileMode {
    None,
    /// The parent writes it right after `spawn`.
    Ours,
    /// Caddy writes it (`caddy run --pidfile`).
    Caddy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StdinMode {
    Null,
    /// A pipe that the parent keeps open (case P4).
    Pipe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Die {
    /// Wait for a signal from the script.
    None,
    /// Panic on the main thread one second after READY.
    PanicMain,
    /// Panic inside the task that owns the `Child`.
    PanicTask,
    /// `main` returns normally.
    Exit,
    /// `std::process::exit(0)`: no `Drop` runs.
    ProcessExit,
}

#[derive(ClapArgs, Clone)]
pub struct Args {
    #[arg(long, value_enum, default_value_t = Group::Own)]
    pub group: Group,
    #[arg(long, value_enum, default_value_t = StdioMode::File)]
    pub stdio: StdioMode,
    #[arg(long, value_enum, default_value_t = OnOff::On)]
    pub kill_on_drop: OnOff,
    #[arg(long, value_enum, default_value_t = PidfileMode::Ours)]
    pub pidfile: PidfileMode,
    #[arg(long, value_enum, default_value_t = StdinMode::Null)]
    pub stdin: StdinMode,
    #[arg(long, value_enum, default_value_t = Die::None)]
    pub die: Die,
    /// The text that the site answers with.
    #[arg(long, default_value = "marsa-spike v1")]
    pub body: String,
}

/// How a Caddy child is started.
#[derive(Clone, Copy)]
pub struct SpawnOpts {
    pub group: Group,
    pub stdio: StdioMode,
    pub kill_on_drop: bool,
    pub pidfile: PidfileMode,
    pub stdin: StdinMode,
}

impl SpawnOpts {
    /// What the recommended strategy uses for every Caddy it starts.
    pub fn recommended() -> Self {
        Self {
            group: Group::Own,
            stdio: StdioMode::File,
            kill_on_drop: true,
            pidfile: PidfileMode::Ours,
            stdin: StdinMode::Null,
        }
    }
}

impl From<&Args> for SpawnOpts {
    fn from(a: &Args) -> Self {
        Self {
            group: a.group,
            stdio: a.stdio,
            kill_on_drop: a.kill_on_drop == OnOff::On,
            pidfile: a.pidfile,
            stdin: a.stdin,
        }
    }
}

/// The last lines that Caddy printed, like the supervisor's ring buffer.
#[derive(Clone, Default)]
pub struct Ring(Arc<Mutex<VecDeque<String>>>);

impl Ring {
    const CAPACITY: usize = 200;

    fn push(&self, line: String) {
        // A poisoned lock only means another thread panicked; the lines are
        // still usable. The lock is never held across an `.await`.
        let mut lines = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == Self::CAPACITY {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

fn pump<R>(reader: R, ring: Ring)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            ring.push(line);
        }
    });
}

/// A started Caddy with everything the owning task must keep alive.
pub struct Spawned {
    pub child: Child,
    pub pid: u32,
    /// Held so that the pipe stays open for as long as the parent lives.
    pub _stdin_keepalive: Option<ChildStdin>,
    pub ring: Ring,
    /// Time between `spawn` returning and the pid file being on disk.
    pub pidfile_delay: Option<Duration>,
}

/// Writes the config, starts Caddy, and (if asked) writes our pid file.
pub async fn spawn_caddy(env: &Env, opts: SpawnOpts, body: &str) -> Result<Spawned> {
    for dir in [
        env.run_dir(),
        env.work.join("logs"),
        env.work.join("xdg-config"),
        env.work.join("xdg-data"),
    ] {
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create {}", dir.display()))?;
    }
    let config = caddy_config::build(env.app_port, env.admin_port, body, true);
    tokio::fs::write(env.config_path(), serde_json::to_vec_pretty(&config)?)
        .await
        .context("write the Caddy config")?;

    let mut cmd = Command::new(&env.caddy);
    cmd.args(["run", "--config"]).arg(env.config_path());
    if opts.pidfile == PidfileMode::Caddy {
        cmd.arg("--pidfile").arg(env.caddy_pidfile());
    }
    // Keep Caddy's own state inside the spike folder.
    cmd.env("XDG_CONFIG_HOME", env.work.join("xdg-config"))
        .env("XDG_DATA_HOME", env.work.join("xdg-data"));
    if opts.group == Group::Own {
        cmd.process_group(0);
    }
    cmd.kill_on_drop(opts.kill_on_drop);
    match opts.stdin {
        StdinMode::Null => cmd.stdin(Stdio::null()),
        StdinMode::Pipe => cmd.stdin(Stdio::piped()),
    };
    match opts.stdio {
        StdioMode::File => {
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(env.log_path())
                .context("open the Caddy log file")?;
            let log2 = log.try_clone().context("duplicate the log file handle")?;
            cmd.stdout(Stdio::from(log)).stderr(Stdio::from(log2));
        }
        StdioMode::Pipe => {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
    }

    let spawned_at = Instant::now();
    let mut child = cmd.spawn().context("spawn Caddy")?;
    let pid = child.id().context("the child has no pid")?;
    let ring = Ring::default();
    if let Some(out) = child.stdout.take() {
        pump(out, ring.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump(err, ring.clone());
    }
    let stdin = child.stdin.take();

    let mut pidfile_delay = None;
    if opts.pidfile == PidfileMode::Ours {
        // The start time costs one `ps` call. The window between `spawn` and
        // the file is the time in which a SIGKILL leaves an orphan with no
        // record, so we measure it.
        let started = sys::started_of(pid).await.unwrap_or_default();
        let record = PidRecord {
            pid,
            exe: env.caddy_exe(),
            started,
        };
        sys::write_atomic(&env.pidfile(), &serde_json::to_string(&record)?).await?;
        pidfile_delay = Some(spawned_at.elapsed());
    }
    Ok(Spawned {
        child,
        pid,
        _stdin_keepalive: stdin,
        ring,
        pidfile_delay,
    })
}

/// Polls the admin API until Caddy answers.
pub async fn wait_ready(env: &Env, limit: Duration) -> Result<Duration> {
    let client = http_client(Duration::from_millis(300))?;
    let start = Instant::now();
    loop {
        if let Ok(resp) = client.get(env.admin_url("/config/")).send().await
            && resp.status().is_success()
        {
            return Ok(start.elapsed());
        }
        if start.elapsed() >= limit {
            bail!("Caddy did not answer on the admin API within {limit:?}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The task that owns the `Child`. It stops it when the token is cancelled.
/// With `panic_after_one_second` it panics instead, to show what happens to a
/// `Child` whose task dies (case D7).
async fn supervise(
    mut sp: Spawned,
    cancel: CancellationToken,
    panic_after_one_second: bool,
) -> std::io::Result<ExitStatus> {
    if panic_after_one_second {
        tokio::time::sleep(Duration::from_secs(1)).await;
        panic!("S06 simulated panic in the task that owns the Child");
    }
    tokio::select! {
        status = sp.child.wait() => status,
        () = cancel.cancelled() => {
            sp.child.start_kill()?;
            sp.child.wait().await
        }
    }
}

pub async fn run(env: &Env, args: Args) -> Result<()> {
    let opts = SpawnOpts::from(&args);
    let sp = spawn_caddy(env, opts, &args.body).await?;
    let caddy_pid = sp.pid;
    if let Some(delay) = sp.pidfile_delay {
        println!("PIDFILE_WRITTEN delay_us={}", delay.as_micros());
    }
    let ready_after = wait_ready(env, Duration::from_secs(10)).await?;
    println!(
        "READY parent={} caddy={caddy_pid} ready_ms={} ring_lines={}",
        std::process::id(),
        ready_after.as_millis(),
        sp.ring.len()
    );

    let cancel = CancellationToken::new();
    let task = tokio::spawn(supervise(sp, cancel.clone(), args.die == Die::PanicTask));
    match args.die {
        Die::None => std::future::pending::<()>().await,
        Die::PanicMain => {
            tokio::time::sleep(Duration::from_secs(1)).await;
            panic!("S06 simulated panic on the main thread");
        }
        Die::Exit => {
            tokio::time::sleep(Duration::from_secs(1)).await;
            // Returning from `main` drops the runtime, which drops the task,
            // which drops the `Child`.
            return Ok(());
        }
        Die::ProcessExit => {
            tokio::time::sleep(Duration::from_secs(1)).await;
            std::process::exit(0);
        }
        Die::PanicTask => {
            // The daemon stays alive. This is everything it learns.
            match task.await {
                Err(e) if e.is_panic() => {
                    let alive = sys::is_alive(caddy_pid).await;
                    println!("DAEMON_SAW join=panic caddy_alive_right_after={alive}");
                }
                other => println!("DAEMON_SAW join=other result={other:?}"),
            }
            std::future::pending::<()>().await;
        }
    }
    Ok(())
}
