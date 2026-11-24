//! What a start does before its run: the version, the check before the
//! start (ADR 0023, section 7), and the first init of each server.

use std::path::{Path, PathBuf};

use marsa_core::config::ServerSettings;
use marsa_core::databases::mailpit::{MAILPIT_UI_PORT, mailpit_database};
use marsa_core::databases::mysql::{
    MysqlDirs, decide_mysql_start, my_cnf, mysql_stop, mysql_windows_stop, mysqld_pid,
};
use marsa_core::databases::valkey::{ValkeyDirs, valkey_conf};
use marsa_core::databases::{
    PostgresDirs, ServerKind, StartDecision, StartFacts, decide_start, postgres_stop_on,
    postmaster_pid, refuses_elevation, socket_for,
};
use marsa_core::recipes::{Os, Platform};
use marsa_core::runtimes::RuntimesDir;
use marsa_core::services::ServiceSpec;
use semver::Version;
use tracing::{info, warn};

use super::super::facts::{port_facts, start_facts};
use super::super::init::{self, InitError, installed_servers};
use super::super::settings::read_settings;
use super::super::{mailpit, mysql, orphan, postgres, valkey};
use super::run::Refusal;
use super::{ActionError, ServerManager};
use crate::daemon::services::pid_record::{self, Cleared};
use crate::daemon::services::supervisor::lock;
use crate::daemon::state::Daemon;

/// The version a start picked, and what goes with it, for the part of
/// [`ServerManager::prepare`] of each server.
struct Chosen {
    platform: Platform,
    version: Version,
    /// The server program of `version`.
    server: PathBuf,
    /// The server program of each installed version, for `run/<id>.pid`.
    servers: Vec<PathBuf>,
    port: u16,
}

/// What a start runs, after the check before it and the first init.
pub(super) struct Prepared {
    pub(super) spec: ServiceSpec,
    /// The server program of each installed version, for `run/<id>.pid`.
    pub(super) servers: Vec<PathBuf>,
    /// What a run that ends by itself is read for.
    pub(super) refusal: Refusal,
    pub(super) port: u16,
}

impl ServerManager {
    /// The version, the check before the start (ADR 0023, section 7), then
    /// the first init when the data folder is new. The check comes first,
    /// so a busy port makes no data folder.
    pub(super) async fn prepare(&self, daemon: &Daemon) -> Result<Prepared, ActionError> {
        let kind = self.kind;
        let settings = read_settings(kind, daemon.data_dir())?;
        let (platform, version, server) = self.installed(daemon.data_dir(), &settings)?;
        // Before the first init too: its single-user `postgres` refuses the
        // token as the server does.
        if refuses_elevation(kind, (self.elevated)()) {
            return Err(ActionError::Elevated(kind));
        }
        let port = settings.port.unwrap_or(kind.default_port());
        let servers = installed_servers(kind, daemon.data_dir(), platform);
        let ladder = &self.orphan_ladder;
        let probes = &*self.probes;
        let looks = pid_record::START_LOOKS;
        let owner = orphan::owner(kind, daemon.data_dir(), port);
        let cleared = pid_record::clear(&owner, probes, &servers, ladder, looks);
        match cleared.await {
            Ok(Cleared::Stopped { pid, signal }) => {
                let text = orphan::stopped_line(kind, pid, signal);
                lock(&self.runtime).note(daemon.events(), &text);
            }
            Ok(Cleared::Nothing | Cleared::Forgotten | Cleared::Unknown { .. }) => {}
            Err(pid_record::Stuck(reason)) => return Err(ActionError::OrphanStuck(kind, reason)),
        }
        let chosen = Chosen {
            platform,
            version,
            server,
            servers,
            port,
        };
        match kind {
            ServerKind::Postgres => self.prepare_postgres(daemon, chosen).await,
            ServerKind::Mysql => self.prepare_mysql(daemon, chosen).await,
            ServerKind::Valkey => self.prepare_valkey(daemon, chosen).await,
            ServerKind::Mailpit => self.prepare_mailpit(daemon, chosen).await,
        }
    }

    /// The check before the start, from the server's own pid file
    /// `pid_file`, by `decide`.
    async fn check(
        &self,
        port: u16,
        pid_file: &Path,
        pid_of: fn(&str) -> Option<u32>,
        servers: &[PathBuf],
        decide: impl Fn(&StartFacts) -> StartDecision,
    ) -> Result<(), ActionError> {
        let facts = start_facts(&*self.probes, port, pid_file, pid_of, servers).await;
        self.decide(&facts, decide)
    }

    /// `decide` on `facts`, the facts before the start.
    fn decide(
        &self,
        facts: &StartFacts,
        decide: impl Fn(&StartFacts) -> StartDecision,
    ) -> Result<(), ActionError> {
        info!(?facts, server = self.kind.id(), "checked before the start");
        match decide(facts) {
            StartDecision::Refuse(busy) => Err(ActionError::InUse(busy)),
            StartDecision::Start => Ok(()),
        }
    }

    /// [`Self::prepare`] for PostgreSQL: the check, `initdb` for a new
    /// cluster, and its socket.
    async fn prepare_postgres(
        &self,
        daemon: &Daemon,
        chosen: Chosen,
    ) -> Result<Prepared, ActionError> {
        let Chosen {
            platform,
            version,
            servers,
            port,
            ..
        } = chosen;
        let os = platform.os();
        let dirs = PostgresDirs::new(daemon.data_dir(), version.major);
        let pid_file = dirs.data.join("postmaster.pid");
        self.check(port, &pid_file, postmaster_pid, &servers, decide_start)
            .await?;
        let programs = postgres::programs(daemon.data_dir(), platform, &version)
            .ok_or_else(|| self.missing(Some(&self.kind.line(&version))))?;
        postgres::ensure_cluster(&programs, &dirs, os)
            .await
            .map_err(ActionError::Init)?;
        info!(%version, port, "starting postgres");
        // `pg_hba.conf` lets the superuser in over the socket only, so
        // without one nobody can connect as `postgres` (ADR 0023, "Access").
        let socket = socket_for(&dirs, port);
        if let Err(why) = &socket {
            let folder = dirs.sockets.display();
            warn!(%folder, %why, "no Unix socket for postgres: TCP only, no superuser");
            let text = format!(
                "marsad: PostgreSQL gets no Unix socket in {folder}: {why}. So it takes TCP \
                 connections only, and its superuser postgres cannot connect."
            );
            lock(&self.runtime).note(daemon.events(), &text);
        }
        let stop = postgres_stop_on(os, &programs.pg_ctl, &dirs.data);
        Ok(Prepared {
            spec: postgres::spec(programs.postgres, &dirs, port, stop),
            servers,
            refusal: Refusal::Postgres {
                data: dirs.data,
                socket: socket.ok(),
            },
            port,
        })
    }

    /// [`Self::prepare`] for MySQL: `my.cnf`, the check (on macOS and Linux
    /// a pid that cannot be told refuses too: [`decide_mysql_start`]), and
    /// the first init for a new data folder.
    ///
    /// On Windows MySQL gets no socket: there `socket` names a named pipe,
    /// which Marsa leaves off, because any local user could open it as
    /// `root@localhost`. It stops with `mysqladmin shutdown` there.
    async fn prepare_mysql(
        &self,
        daemon: &Daemon,
        chosen: Chosen,
    ) -> Result<Prepared, ActionError> {
        let Chosen {
            platform,
            version,
            server: mysqld,
            servers,
            port,
        } = chosen;
        let kind = self.kind;
        let os = platform.os();
        let dirs = MysqlDirs::new(daemon.data_dir(), &kind.line(&version));
        let socket = (os != Os::Windows).then(|| dirs.socket());
        let socket_path = socket.as_ref().and_then(|socket| socket.as_deref().ok());
        let basedir = RuntimesDir::in_dir(daemon.data_dir()).version_dir(kind.runtime(), &version);
        let text = my_cnf(&dirs, &basedir, port, socket_path).map_err(ActionError::Path)?;
        let stop = if os == Os::Windows {
            let mysqladmin =
                init::program(kind, daemon.data_dir(), platform, &version, "mysqladmin")
                    .ok_or_else(|| self.missing(Some(&kind.line(&version))))?;
            mysql_windows_stop(&mysqladmin, port)
        } else {
            mysql_stop()
        };
        let pid_file = dirs.pid_file();
        let decide = |facts: &StartFacts| decide_mysql_start(facts, os);
        self.check(port, &pid_file, mysqld_pid, &servers, decide)
            .await?;
        mysql::write_config(&dirs, text)
            .await
            .map_err(|err| ActionError::Init(InitError::io(kind, "cannot write my.cnf", &err)))?;
        mysql::ensure_data_dir(&mysqld, &dirs, os)
            .await
            .map_err(ActionError::Init)?;
        info!(%version, port, "starting mysql");
        // Without a socket nobody reaches `root@localhost`, the account
        // with every privilege (ADR 0023, "Access").
        if let Some(Err(why)) = &socket {
            let folder = dirs.run.display();
            warn!(%folder, %why, "no Unix socket for mysql: TCP only, no root@localhost");
            let text = format!(
                "marsad: MySQL gets no Unix socket in {folder}: {why}. So it takes TCP \
                 connections only, and root@localhost cannot connect."
            );
            lock(&self.runtime).note(daemon.events(), &text);
        }
        Ok(Prepared {
            spec: mysql::spec(mysqld, &dirs, os, stop),
            servers,
            refusal: Refusal::None,
            port,
        })
    }

    /// [`Self::prepare`] for Valkey: `valkey.conf`, and the check of its
    /// port. Valkey has no pid file of its own and no lock on its data
    /// folder, so the port is the whole check, after our own record of its
    /// pid ([`pid_record::clear`]). A program that holds the port, Homebrew's
    /// Redis or Herd's, refuses the start with its name. No first init:
    /// Valkey starts on an empty folder.
    async fn prepare_valkey(
        &self,
        daemon: &Daemon,
        chosen: Chosen,
    ) -> Result<Prepared, ActionError> {
        let Chosen {
            platform,
            version,
            server,
            servers,
            port,
        } = chosen;
        let kind = self.kind;
        let os = platform.os();
        let dirs = ValkeyDirs::new(daemon.data_dir());
        let text = valkey_conf(&dirs, port, os).map_err(ActionError::Path)?;
        let spec = valkey::spec(server, &dirs, port, os).map_err(ActionError::Path)?;
        let facts = port_facts(&*self.probes, port).await;
        self.decide(&facts, decide_start)?;
        valkey::write_config(&dirs, text).await.map_err(|err| {
            ActionError::Init(InitError::io(kind, "cannot write valkey.conf", &err))
        })?;
        info!(%version, port, "starting valkey");
        Ok(Prepared {
            spec,
            servers,
            refusal: Refusal::None,
            port,
        })
    }

    /// [`Self::prepare`] for Mailpit: the check of both its ports, SMTP and
    /// the web page, then its mail folder. Mailpit has no pid file of its own
    /// and no lock, so the ports are the whole check, after our own record of
    /// its pid ([`pid_record::clear`]). Another mail catcher on either port
    /// (Herd's, Docker's) refuses the start with its name.
    async fn prepare_mailpit(
        &self,
        daemon: &Daemon,
        chosen: Chosen,
    ) -> Result<Prepared, ActionError> {
        let Chosen {
            version,
            server,
            servers,
            port,
            ..
        } = chosen;
        let kind = self.kind;
        for checked in [port, MAILPIT_UI_PORT] {
            let facts = port_facts(&*self.probes, checked).await;
            self.decide(&facts, decide_start)?;
        }
        let database = mailpit_database(daemon.data_dir());
        mailpit::make_folder(&database).await.map_err(|err| {
            ActionError::Init(InitError::io(kind, "cannot make the mail folder", &err))
        })?;
        info!(%version, port, "starting mailpit");
        Ok(Prepared {
            spec: mailpit::spec(server, port, &database),
            servers,
            refusal: Refusal::None,
            port,
        })
    }

    /// The server program to run for `settings`: the newest installed
    /// version of the `default` line, or of all lines when it is not set.
    pub(in crate::daemon::services::databases) fn installed(
        &self,
        data_dir: &Path,
        settings: &ServerSettings,
    ) -> Result<(Platform, Version, PathBuf), ActionError> {
        let kind = self.kind;
        let platform = match self.platform {
            Some(platform) if platform != Platform::Any => platform,
            _ => return Err(ActionError::Unsupported(kind)),
        };
        let line = settings.default.as_deref();
        let installed = RuntimesDir::in_dir(data_dir)
            .installed(kind.runtime())
            .map_err(|err| ActionError::NotInstalled(kind, err.to_string()))?;
        let version = kind
            .line_version(line, &installed)
            .ok_or_else(|| self.missing(line))?;
        let server = init::server(kind, data_dir, platform, &version)
            .filter(|server| server.is_file())
            .ok_or_else(|| self.missing(line))?;
        Ok((platform, version, server))
    }

    /// No version of `line` (or of any line) is installed.
    fn missing(&self, line: Option<&str>) -> ActionError {
        let what = line.map_or_else(String::new, |line| format!(" {line}"));
        let reason = format!("no {}{what} is installed", self.kind.name());
        ActionError::NotInstalled(self.kind, reason)
    }
}
