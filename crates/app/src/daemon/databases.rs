//! PostgreSQL as a service of `marsad` (ADR 0023, task M6-03), on macOS and
//! Linux. `marsa_core::databases` decides; this module runs the programs.
//!
//! The first start of a major line makes its cluster in
//! `databases/postgres/<major>/` ([`ensure_cluster`]): `initdb`, then Marsa's
//! `pg_hba.conf`, then the role `root` in single-user mode. All of it happens
//! in a temp folder next to the target, which is renamed into place at the
//! end, so a cluster that is half made is never taken for a ready one.
//!
//! Before each start, [`start_facts`] reads the port and `postmaster.pid`
//! from the system for `decide_start`. It only reads: nothing here signals a
//! process or deletes `postmaster.pid`.

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use marsa_core::databases::{
    PG_HBA_CONF, PidFileCheck, PostgresDirs, ROOT_ROLE_SQL, StartFacts, classify_postmaster,
    initdb_args, port_check, postmaster_pid, single_user_args,
};
use marsa_core::orphan::Liveness;
use marsa_core::platform::{probe, process};
use marsa_core::ports::PortLookup;
use marsa_core::recipes::{self, Platform};
use marsa_core::runtimes::{Runtime, RuntimesDir};
use semver::Version;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use tracing::{info, warn};

use super::caddy::orphan::BoxFuture;
use super::php::orphan::accepted;

/// How long `initdb` may run. About a second on a laptop (S-12); a slow disk
/// or a busy CI machine needs more.
const INITDB_LIMIT: Duration = Duration::from_secs(120);
/// How long the single-user `postgres` that makes `root` may run.
const SINGLE_USER_LIMIT: Duration = Duration::from_secs(60);
/// The last lines of a failed step's output kept in its error.
const ERROR_LINES: usize = 10;

/// The programs of one installed PostgreSQL version. Absolute: `marsad` never
/// searches `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Programs {
    pub postgres: PathBuf,
    pub initdb: PathBuf,
}

/// Where the recipe puts the programs of the installed PostgreSQL `version`:
/// `runtimes/postgres/<version>/bin/`. `None` if the recipe has none for
/// `platform`.
pub fn programs(data_dir: &Path, platform: Platform, version: &Version) -> Option<Programs> {
    // A test parses every built-in recipe, so an error here never ships.
    let recipe = recipes::builtin(Runtime::Postgres)
        .ok()?
        .platform(platform)?;
    let dir = RuntimesDir::in_dir(data_dir).version_dir(Runtime::Postgres, version);
    Some(Programs {
        postgres: recipe.program_path(&dir, "postgres")?,
        initdb: recipe.program_path(&dir, "initdb")?,
    })
}

/// `bin/postgres` of every installed PostgreSQL version, and the resolved
/// form of each: the programs that a `postmaster.pid` of this data folder
/// may name. An older version may still run on the folder. A failure to list
/// the versions gives fewer paths, so fewer pids count as ours.
pub fn installed_servers(data_dir: &Path, platform: Platform) -> Vec<PathBuf> {
    let versions = RuntimesDir::in_dir(data_dir)
        .installed(Runtime::Postgres)
        .unwrap_or_else(|err| {
            warn!(code = err.code(), %err, "cannot list the PostgreSQL versions");
            Vec::new()
        });
    let servers = versions
        .iter()
        .filter_map(|version| programs(data_dir, platform, version))
        .map(|programs| programs.postgres);
    accepted(servers)
}

/// Who runs under a pid, and who listens on a port: the OS in the daemon
/// ([`SystemProbes`]), a fake in tests. Only asks; never signals.
pub trait Probes: fmt::Debug + Send + Sync {
    /// By the file the process really runs:
    /// [`marsa_core::platform::process::real_identity`].
    fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness>;
    /// Who listens on the port, on any address:
    /// [`marsa_core::platform::process::port_lookup`]. An owner, or a look
    /// that failed, means busy.
    fn port_lookup(&self, port: u16) -> BoxFuture<'_, PortLookup>;
}

/// The real OS.
#[derive(Debug)]
pub struct SystemProbes;

impl Probes for SystemProbes {
    fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::real_identity(pid))
    }

    fn port_lookup(&self, port: u16) -> BoxFuture<'_, PortLookup> {
        Box::pin(process::port_lookup(port))
    }
}

/// The facts `decide_start` needs, read from the system now:
///
/// - the port: a bind to `127.0.0.1:<port>`, and who listens on it on any
///   address; a look that fails counts as busy ([`port_check`]);
/// - `postmaster.pid` in the cluster `data`: read with a size cap, never
///   through a link ([`process::read_small_file`]), and the program at its
///   pid, by its real executable ([`classify_postmaster`] against
///   `servers`, from [`installed_servers`]).
///
/// The caller first makes sure that our own supervisor does not run
/// PostgreSQL: that one holds the port and the file, and is not busy.
pub async fn start_facts(
    probes: &dyn Probes,
    port: u16,
    data: &Path,
    servers: &[PathBuf],
) -> StartFacts {
    // The listener closes at once.
    let bound = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
    let lookup = probes.port_lookup(port).await;
    let text = process::read_small_file(&data.join("postmaster.pid"));
    let pid_file = match text.as_deref().and_then(postmaster_pid) {
        None => PidFileCheck::NoPid,
        Some(pid) => classify_postmaster(pid, &probes.real_identity(pid).await, servers),
    };
    StartFacts {
        port,
        port_check: port_check(bound, lookup),
        pid_file,
    }
}

/// Why the first init of a cluster failed. Nothing is left behind: no
/// cluster, and no temp folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitError {
    pub reason: String,
}

impl InitError {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        "postgres_init_failed"
    }

    fn io(what: &str, err: &io::Error) -> Self {
        Self {
            reason: format!("{what}: {err}"),
        }
    }
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the PostgreSQL data folder was not made: {}",
            self.reason
        )
    }
}

impl std::error::Error for InitError {}

/// Makes the cluster in `dirs.data` if nothing is there yet, and returns
/// whether it made one. Anything at that path, even an empty folder or a
/// link, counts as there: it is never read, run, or changed here, and
/// PostgreSQL says at its start whether it is a cluster.
///
/// The steps, all in a new temp folder `.<major>.init-<random>` next to the
/// target:
///
/// 1. `initdb` with [`initdb_args`];
/// 2. [`PG_HBA_CONF`] over initdb's `pg_hba.conf`;
/// 3. `postgres --single` with [`ROOT_ROLE_SQL`] on stdin;
/// 4. a rename to `dirs.data`, on the same volume, so the cluster appears
///    whole or not at all, then a sync of the folder that holds it.
///
/// Temp folders that an earlier init left (a crash, a power cut) are deleted
/// first: none of them was ever a cluster in use.
///
/// # Errors
///
/// `postgres_init_failed`, with the step and the end of its output. The temp
/// folder is deleted then.
pub async fn ensure_cluster(programs: &Programs, dirs: &PostgresDirs) -> Result<bool, InitError> {
    match tokio::fs::symlink_metadata(&dirs.data).await {
        Ok(_) => return Ok(false),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(InitError::io("cannot look at the data folder", &err)),
    }
    let (Some(parent), Some(name)) = (dirs.data.parent(), dirs.data.file_name()) else {
        let reason = format!("{} has no parent folder", dirs.data.display());
        return Err(InitError { reason });
    };
    let prefix = format!(".{}.init-", name.to_string_lossy());
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|err| InitError::io("cannot create the databases folder", &err))?;
    remove_leftovers(parent, &prefix).await;
    let temp = parent.join(format!("{prefix}{:016x}", probe::random()));
    info!(data = %dirs.data.display(), "making the PostgreSQL data folder");
    let made = init_in(programs, dirs, &temp).await;
    if made.is_err()
        && let Err(err) = remove_dir(&temp).await
    {
        warn!(temp = %temp.display(), %err, "cannot delete the temp folder of a failed init");
    }
    made.map(|()| true)
}

/// Steps 1 to 4 of [`ensure_cluster`] in `temp`.
async fn init_in(programs: &Programs, dirs: &PostgresDirs, temp: &Path) -> Result<(), InitError> {
    let in_temp = PostgresDirs {
        data: temp.to_path_buf(),
        sockets: dirs.sockets.clone(),
    };
    let mut initdb = Command::new(&programs.initdb);
    initdb.args(initdb_args(&in_temp));
    run_step(initdb, None, INITDB_LIMIT, "initdb").await?;

    write_hba(&temp.join("pg_hba.conf"))
        .await
        .map_err(|err| InitError::io("cannot write pg_hba.conf", &err))?;

    let mut single = Command::new(&programs.postgres);
    single.args(single_user_args(temp));
    let sql = Some(ROOT_ROLE_SQL);
    run_step(single, sql, SINGLE_USER_LIMIT, "postgres --single").await?;

    // Checked again: a rename would replace an empty folder made meanwhile.
    if tokio::fs::symlink_metadata(&dirs.data).await.is_ok() {
        let reason = format!("{} appeared during the init", dirs.data.display());
        return Err(InitError { reason });
    }
    tokio::fs::rename(temp, &dirs.data)
        .await
        .map_err(|err| InitError::io("cannot move the new cluster into place", &err))?;
    // The rename is on disk only once its folder is: after a crash before
    // that, the cluster could be back under its temp name, which the next
    // init deletes as a leftover.
    let parent = dirs.data.parent().map(Path::to_path_buf);
    tokio::task::spawn_blocking(move || parent.map_or(Ok(()), |parent| sync_dir(&parent)))
        .await
        .map_err(io::Error::other)
        .flatten()
        .map_err(|err| InitError::io("cannot sync the databases folder", &err))?;
    info!(data = %dirs.data.display(), "the PostgreSQL data folder is ready");
    Ok(())
}

/// Flushes the entries of `dir` (a rename into it) to disk.
// `#[cfg(unix)]` outside `platform`, as in the installer: a folder's entries
// are flushed by opening the folder, which only Unix allows. NTFS logs them
// itself.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Writes Marsa's `pg_hba.conf` over initdb's, which keeps its mode (`0600`),
/// and syncs it: the cluster is renamed into place next.
async fn write_hba(path: &Path) -> io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .await?;
    file.write_all(PG_HBA_CONF.as_bytes()).await?;
    file.sync_all().await
}

/// Runs one step: `input` on stdin, then waits up to `limit`. A step that
/// runs longer is killed (`kill_on_drop`).
///
/// It fails on an exit code other than 0, or on a line with `ERROR:`,
/// `FATAL:`, or `PANIC:`, PostgreSQL's severities: a check on the output too,
/// in case a session went on after an error.
async fn run_step(
    mut cmd: Command,
    input: Option<&str>,
    limit: Duration,
    what: &str,
) -> Result<(), InitError> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    process::hide_console_window(&mut cmd);
    let failed = |reason: String| InitError {
        reason: format!("{what} {reason}"),
    };
    let mut child = cmd
        .spawn()
        .map_err(|err| failed(format!("did not start: {err}")))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // Dropped at the end of the block: the step reads to the end.
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|err| failed(format!("did not read its input: {err}")))?;
    }
    let output = tokio::time::timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| failed(format!("took longer than {} s", limit.as_secs())))?
        .map_err(|err| failed(format!("cannot be waited for: {err}")))?;
    let text = [&output.stderr, &output.stdout]
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .join("\n");
    let severe = ["ERROR:", "FATAL:", "PANIC:"];
    let error_line = text
        .lines()
        .any(|line| severe.iter().any(|s| line.contains(s)));
    if output.status.success() && !error_line {
        return Ok(());
    }
    let lines: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(ERROR_LINES)..].join(" | ");
    Err(failed(format!("failed ({}): {tail}", output.status)))
}

/// Deletes the temp folders of earlier inits of this major in `parent`: the
/// folders whose name starts with `prefix`. Never one of another major, and
/// never a link (`file_type` does not follow it).
async fn remove_leftovers(parent: &Path, prefix: &str) {
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let ours = entry.file_name().to_string_lossy().starts_with(prefix);
        if !ours || !entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        match remove_dir(&path).await {
            Ok(()) => info!(temp = %path.display(), "deleted the temp folder of an earlier init"),
            Err(err) => warn!(temp = %path.display(), %err, "cannot delete an old temp folder"),
        }
    }
}

/// Deletes `dir` and what is in it. Nothing there is not an error.
async fn remove_dir(dir: &Path) -> io::Result<()> {
    match tokio::fs::remove_dir_all(dir).await {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use marsa_core::databases::{PortCheck, ServiceBusy, StartDecision, decide_start};

    /// Answers what it was given, and keeps the pids it was asked about.
    #[derive(Debug)]
    struct FakeProbes {
        now: Liveness,
        lookup: PortLookup,
        asked: Mutex<Vec<u32>>,
    }

    /// `owner` listens on the port, or nobody.
    fn probes(now: Liveness, owner: Option<&str>) -> FakeProbes {
        let lookup = owner.map_or(PortLookup::Nobody, |owner| PortLookup::Owner(owner.into()));
        FakeProbes {
            now,
            lookup,
            asked: Mutex::default(),
        }
    }

    impl Probes for FakeProbes {
        fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
            self.asked.lock().unwrap().push(pid);
            Box::pin(std::future::ready(self.now.clone()))
        }

        fn port_lookup(&self, _port: u16) -> BoxFuture<'_, PortLookup> {
            Box::pin(std::future::ready(self.lookup.clone()))
        }
    }

    fn alive(exe: &Path) -> Liveness {
        Liveness::Alive {
            exe: exe.to_path_buf(),
            started: "Wed Nov 18 10:00:00 2026".into(),
        }
    }

    /// A port nothing listens on now.
    fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    const POSTMASTER_PID: &str = "4242\n/m/databases/postgres/18\n1763460000\n5432\n/m/run\n\
                                  127.0.0.1\n  5432001         0\nready   \n";

    #[tokio::test]
    async fn the_port_is_busy_when_the_bind_fails_or_lsof_names_an_owner() {
        let data = tempfile::tempdir().unwrap();
        let facts = async |port, owner| {
            let probes = probes(Liveness::Gone, owner);
            start_facts(&probes, port, data.path(), &[])
                .await
                .port_check
        };
        let port = free_port();
        assert_eq!(facts(port, None).await, PortCheck::Free);
        // Docker on `*:<port>`: our bind works on macOS, but lsof sees it.
        let docker = Some("com.docker.backend(77)");
        let busy = PortCheck::Busy {
            owner: docker.map(str::to_owned),
        };
        assert_eq!(facts(port, docker).await, busy);
        // A listener on `127.0.0.1:<port>`, and no lsof (Linux).
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let busy = PortCheck::Busy { owner: None };
        assert_eq!(facts(port, None).await, busy);
    }

    /// macOS: when lsof fails or takes too long, a holder of `*:<port>`
    /// could be there unseen, so the port counts as busy.
    #[tokio::test]
    async fn a_failed_look_at_the_port_is_busy() {
        let data = tempfile::tempdir().unwrap();
        let mut failed = probes(Liveness::Gone, None);
        failed.lookup = PortLookup::Failed;
        let facts = start_facts(&failed, free_port(), data.path(), &[]).await;
        assert_eq!(facts.port_check, PortCheck::Busy { owner: None });
    }

    #[tokio::test]
    async fn postmaster_pid_is_read_and_its_pid_classified() {
        let data = tempfile::tempdir().unwrap();
        let servers = [data.path().join("runtimes/postgres/18.7.0/bin/postgres")];
        let check = async |now: Liveness| {
            let probes = probes(now, None);
            let facts = start_facts(&probes, free_port(), data.path(), &servers).await;
            (facts.pid_file, probes.asked.into_inner().unwrap())
        };
        // No file: nobody is asked about any pid.
        let gone = || Liveness::Gone;
        assert_eq!(check(gone()).await, (PidFileCheck::NoPid, vec![]));

        let file = data.path().join("postmaster.pid");
        std::fs::write(&file, POSTMASTER_PID).unwrap();
        let pid = 4242;
        let server = check(alive(&servers[0])).await;
        assert_eq!(server, (PidFileCheck::Server { pid }, vec![pid]));
        let other = check(alive(Path::new("/bin/sleep"))).await.0;
        assert_eq!(other, PidFileCheck::Other { pid });
        assert_eq!(check(gone()).await.0, PidFileCheck::Dead { pid });
        let unknown = check(Liveness::Unknown).await.0;
        assert_eq!(unknown, PidFileCheck::Unknown { pid });

        // Junk, and a file too long to be PostgreSQL's: no pid.
        let long = format!("{POSTMASTER_PID}{}", "x".repeat(5000));
        for text in ["", "abc\n", "0\n", long.as_str()] {
            std::fs::write(&file, text).unwrap();
            assert_eq!(
                check(alive(&servers[0])).await,
                (PidFileCheck::NoPid, vec![])
            );
        }
    }

    /// A link is never followed, even to a real `postmaster.pid`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_linked_postmaster_pid_names_no_pid() {
        let data = tempfile::tempdir().unwrap();
        let real = data.path().join("elsewhere.pid");
        std::fs::write(&real, POSTMASTER_PID).unwrap();
        std::os::unix::fs::symlink(&real, data.path().join("postmaster.pid")).unwrap();
        let probes = probes(Liveness::Gone, None);
        let facts = start_facts(&probes, free_port(), data.path(), &[]).await;
        assert_eq!(facts.pid_file, PidFileCheck::NoPid);
        assert!(probes.asked.into_inner().unwrap().is_empty());
    }

    /// The facts as the system gives them, through `decide_start`.
    #[tokio::test]
    async fn real_facts_lead_to_their_decision() {
        let data = tempfile::tempdir().unwrap();
        let ours = data.path().join("runtimes/postgres/17.12.0/bin/postgres");
        let servers = [ours.clone()];
        std::fs::write(data.path().join("postmaster.pid"), POSTMASTER_PID).unwrap();
        let decide = async |port, now, owner| {
            let probes = probes(now, owner);
            decide_start(&start_facts(&probes, port, data.path(), &servers).await)
        };
        // Our older version still runs on the folder: refused, whatever
        // holds the port.
        let busy = StartDecision::Refuse(ServiceBusy::DataDir { pid: 4242 });
        assert_eq!(decide(free_port(), alive(&ours), None).await, busy);
        // A stale file, or a pid that another program has now: PostgreSQL
        // decides at its start.
        let sleep = alive(Path::new("/bin/sleep"));
        assert_eq!(decide(free_port(), sleep, None).await, StartDecision::Start);
        assert_eq!(
            decide(free_port(), Liveness::Gone, None).await,
            StartDecision::Start
        );
        // Homebrew's PostgreSQL holds the port.
        let port = free_port();
        let owner = Some("postgres(901)");
        let refused = StartDecision::Refuse(ServiceBusy::Port {
            port,
            owner: owner.map(str::to_owned),
        });
        assert_eq!(decide(port, Liveness::Gone, owner).await, refused);
    }

    #[test]
    fn installed_servers_are_each_versions_postgres() {
        let dir = tempfile::tempdir().unwrap();
        let platform = Platform::current().unwrap();
        let version = |text: &str| Version::parse(text).unwrap();
        let mut want = Vec::new();
        for text in ["17.12.0", "18.7.0"] {
            let programs = programs(dir.path(), platform, &version(text)).unwrap();
            std::fs::create_dir_all(programs.postgres.parent().unwrap()).unwrap();
            std::fs::write(&programs.postgres, "").unwrap();
            assert!(
                programs
                    .initdb
                    .starts_with(programs.postgres.parent().unwrap())
            );
            want.push(programs.postgres);
        }
        let servers = installed_servers(dir.path(), platform);
        for program in &want {
            assert!(servers.contains(program), "{servers:?}");
        }
        let mut names = servers.iter().filter_map(|path| path.file_stem());
        assert!(names.all(|name| name == "postgres"));
    }

    /// Shell scripts stand in for `initdb` and `postgres`: macOS and Linux
    /// only, the systems of this task (Windows is task M6-07).
    #[cfg(unix)]
    mod init {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        use tempfile::TempDir;

        /// A data folder and the fakes. The fakes write what they were given
        /// to `calls.log` and `stdin.log` in `dir`.
        struct Fakes {
            dir: TempDir,
            programs: Programs,
            dirs: PostgresDirs,
        }

        /// `initdb` makes `-D <folder>` with a `PG_VERSION` and its own
        /// `pg_hba.conf`. `postgres` keeps the `pg_hba.conf` it found and
        /// its stdin, prints `print`, and exits with `exit`.
        fn fakes(print: &str, exit: u8) -> Fakes {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("runtimes/postgres/18.7.0/bin");
            fs::create_dir_all(&bin).unwrap();
            let at = |name: &str| dir.path().join(name).display().to_string();
            let initdb = format!(
                "#!/bin/sh\necho \"initdb $*\" >> '{calls}'\n[ \"$1\" = -D ] || exit 2\n\
                 mkdir \"$2\" || exit 3\necho 18 > \"$2/PG_VERSION\"\n\
                 echo 'host all all 127.0.0.1/32 trust' > \"$2/pg_hba.conf\"\n",
                calls = at("calls.log"),
            );
            let postgres = format!(
                "#!/bin/sh\necho \"postgres $*\" >> '{calls}'\n\
                 cp \"$3/pg_hba.conf\" '{hba}'\ncat > '{stdin}'\necho '{print}' >&2\nexit {exit}\n",
                calls = at("calls.log"),
                hba = at("hba-at-single.conf"),
                stdin = at("stdin.log"),
            );
            for (name, script) in [("initdb", initdb), ("postgres", postgres)] {
                let path = bin.join(name);
                fs::write(&path, script).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let programs = Programs {
                postgres: bin.join("postgres"),
                initdb: bin.join("initdb"),
            };
            let dirs = PostgresDirs::new(&dir.path().join("app"), 18);
            Fakes {
                dir,
                programs,
                dirs,
            }
        }

        impl Fakes {
            fn read(&self, name: &str) -> String {
                fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
            }

            /// The names in `databases/postgres/`.
            fn listing(&self) -> Vec<String> {
                let parent = self.dirs.data.parent().unwrap();
                let mut names: Vec<_> = fs::read_dir(parent)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names
            }
        }

        #[tokio::test]
        async fn the_first_init_runs_initdb_then_makes_root_then_moves_the_folder() {
            let fakes = fakes("LOG:  database system is shut down", 0);
            assert_eq!(ensure_cluster(&fakes.programs, &fakes.dirs).await, Ok(true));

            let calls = fakes.read("calls.log");
            let calls: Vec<_> = calls.lines().collect();
            let [initdb, single] = calls[..] else {
                panic!("{calls:?}");
            };
            // The same temp folder for both steps, next to the target.
            let temp = initdb.split(' ').nth(2).unwrap();
            let parent = fakes.dirs.data.parent().unwrap();
            let prefix = parent.join(".18.init-").display().to_string();
            assert!(temp.starts_with(&prefix), "{temp}");
            let in_temp = PostgresDirs {
                data: temp.into(),
                sockets: fakes.dirs.sockets.clone(),
            };
            assert_eq!(
                initdb,
                format!("initdb {}", initdb_args(&in_temp).join(" "))
            );
            let want = format!("postgres {}", single_user_args(Path::new(temp)).join(" "));
            assert_eq!(single, want);
            // Our pg_hba.conf was in place before root was made.
            assert_eq!(fakes.read("hba-at-single.conf"), PG_HBA_CONF);
            assert_eq!(fakes.read("stdin.log"), ROOT_ROLE_SQL);

            // Moved into place whole, and nothing else is left.
            let hba = fs::read_to_string(fakes.dirs.data.join("pg_hba.conf")).unwrap();
            assert_eq!(hba, PG_HBA_CONF);
            assert!(fakes.dirs.data.join("PG_VERSION").is_file());
            assert_eq!(fakes.listing(), ["18"]);

            // The next start makes nothing.
            assert_eq!(
                ensure_cluster(&fakes.programs, &fakes.dirs).await,
                Ok(false)
            );
            assert_eq!(fakes.read("calls.log").lines().count(), 2);
        }

        #[tokio::test]
        async fn anything_at_the_data_folder_is_left_as_it_is() {
            let fakes = fakes("", 0);
            let parent = fakes.dirs.data.parent().unwrap();
            fs::create_dir_all(parent).unwrap();
            // An empty folder, a file, and a link: none is touched or run on.
            let data = &fakes.dirs.data;
            for kind in ["folder", "file", "link"] {
                match kind {
                    "folder" => fs::create_dir(data).unwrap(),
                    "file" => fs::write(data, "x").unwrap(),
                    _ => std::os::unix::fs::symlink("/nowhere", data).unwrap(),
                }
                let before = fs::symlink_metadata(data).unwrap();
                assert_eq!(
                    ensure_cluster(&fakes.programs, &fakes.dirs).await,
                    Ok(false)
                );
                let after = fs::symlink_metadata(data).unwrap();
                assert_eq!(before.file_type(), after.file_type(), "{kind}");
                assert_eq!(before.modified().unwrap(), after.modified().unwrap());
                if kind == "folder" {
                    fs::remove_dir(data).unwrap();
                } else {
                    fs::remove_file(data).unwrap();
                }
            }
            assert_eq!(fakes.read("calls.log"), "", "no program ran");
            assert_eq!(fakes.listing(), Vec::<String>::new());
        }

        #[tokio::test]
        async fn a_failed_step_leaves_no_cluster_and_no_temp_folder() {
            // An error that the session went on after, and an error exit.
            for (print, exit) in [
                ("ERROR:  role \"root\" already exists", 0),
                ("FATAL:  could not open file", 1),
            ] {
                let fakes = fakes(print, exit);
                let err = ensure_cluster(&fakes.programs, &fakes.dirs)
                    .await
                    .unwrap_err();
                assert_eq!(err.code(), "postgres_init_failed");
                assert!(err.reason.starts_with("postgres --single failed"), "{err}");
                assert!(err.reason.contains(print), "{err}");
                assert!(!fakes.dirs.data.exists());
                assert_eq!(fakes.listing(), Vec::<String>::new());
            }
        }

        #[tokio::test]
        async fn a_failed_initdb_runs_nothing_after_it() {
            let fakes = fakes("", 0);
            let script = "#!/bin/sh\necho 'initdb: error: cannot be run as root' >&2\nexit 1\n";
            fs::write(&fakes.programs.initdb, script).unwrap();
            let err = ensure_cluster(&fakes.programs, &fakes.dirs)
                .await
                .unwrap_err();
            assert!(err.reason.contains("initdb failed"), "{err}");
            assert!(err.reason.contains("cannot be run as root"), "{err}");
            assert_eq!(fakes.read("stdin.log"), "", "no single-user step");
            assert!(!fakes.dirs.data.exists());
        }

        #[tokio::test]
        async fn temp_folders_of_earlier_inits_of_this_major_are_deleted() {
            let fakes = fakes("", 0);
            let parent = fakes.dirs.data.parent().unwrap();
            for name in [".18.init-dead", ".17.init-dead"] {
                fs::create_dir_all(parent.join(name).join("base")).unwrap();
            }
            assert_eq!(ensure_cluster(&fakes.programs, &fakes.dirs).await, Ok(true));
            assert_eq!(fakes.listing(), [".17.init-dead", "18"]);
        }
    }
}
