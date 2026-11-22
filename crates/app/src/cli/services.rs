//! `marsa services`: the table of the services the daemon manages, and
//! `marsa services start|stop|restart <id>`.

use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::DaemonClient;
use marsa_core::config::{self, ConfigPaths};
use marsa_core::databases::ServerKind;
use marsa_core::types::{ServiceAction, ServiceId, ServiceState, ServiceStatus};

use super::Failure;
use super::daemon::{connect, failure};
use super::strings;

/// The arguments of `marsa services`. Without a command: the table.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct ServicesArgs {
    #[command(subcommand)]
    pub command: Option<ServicesCommand>,
}

/// The commands of `marsa services`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Subcommand)]
pub enum ServicesCommand {
    /// Start a service and wait until it is ready.
    Start {
        /// The service, such as caddy.
        id: String,
    },
    /// Stop a service.
    Stop {
        /// The service, such as caddy.
        id: String,
    },
    /// Stop a service, then start it again.
    Restart {
        /// The service, such as caddy.
        id: String,
    },
}

impl ServicesCommand {
    /// The action and the service id.
    pub fn action(&self) -> (ServiceAction, &str) {
        match self {
            ServicesCommand::Start { id } => (ServiceAction::Start, id),
            ServicesCommand::Stop { id } => (ServiceAction::Stop, id),
            ServicesCommand::Restart { id } => (ServiceAction::Restart, id),
        }
    }
}

/// Starts the daemon if needed, then prints the table, or runs the action and
/// prints the new state. Exit code 1 if the action fails, or if a start or
/// restart leaves the service failed.
pub async fn run(args: &ServicesArgs, data_dir: &Path) -> Result<ExitCode, Failure> {
    let client = connect(data_dir).await?;
    match &args.command {
        None => list(&client, data_dir).await,
        Some(command) => act(&client, command, data_dir).await,
    }
}

async fn list(client: &DaemonClient, data_dir: &Path) -> Result<ExitCode, Failure> {
    match client.services().await {
        Ok(services) => {
            print!("{}", render(&services, data_dir));
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => Err(failure(&err, data_dir, None)),
    }
}

async fn act(
    client: &DaemonClient,
    command: &ServicesCommand,
    data_dir: &Path,
) -> Result<ExitCode, Failure> {
    let (action, id) = command.action();
    let status = match client.service_action(id, action).await {
        Ok(status) => status,
        Err(err) => return Err(failure(&err, data_dir, Some(id))),
    };
    // After a stop the service is not running, which is what was asked; a
    // failure from before is shown under the table.
    let failed = match (&status.state, action) {
        (ServiceState::Failed { code }, ServiceAction::Start | ServiceAction::Restart) => {
            Some(Failure::new(code, data_dir, Some(&status.id.to_string())))
        }
        _ => None,
    };
    match failed {
        Some(failure) => {
            print!("{}", table(std::slice::from_ref(&status)));
            Err(failure)
        }
        None => {
            print!("{}", render(std::slice::from_ref(&status), data_dir));
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The table, then one line per failed service that says why it failed,
/// then, after a blank line each, how to connect to each database server
/// while it runs: lines to copy into a Laravel project's settings.
pub fn render(services: &[ServiceStatus], data_dir: &Path) -> String {
    if services.is_empty() {
        return format!("{}\n", strings::NO_SERVICES);
    }
    let mut text = table(services);
    for status in services {
        if let ServiceState::Failed { code } = &status.state {
            let id = status.id.to_string();
            let failure = Failure::new(code, data_dir, Some(&id));
            text.push_str(&strings::service_failed(
                &id,
                &failure.message,
                &failure.code,
            ));
            text.push('\n');
        }
    }
    let global = config::load_global(&ConfigPaths::in_dir(data_dir).global).ok();
    for kind in ServerKind::ALL {
        let running = services.iter().any(|status| {
            status.id == ServiceId::Database(kind) && status.state == ServiceState::Running
        });
        if !running {
            continue;
        }
        // The port the daemon gave it: `[<id>] port`, read only.
        let port = global
            .as_ref()
            .and_then(|global| kind.settings(global).port);
        let port = port.unwrap_or(kind.default_port());
        let lines = match kind {
            ServerKind::Postgres => strings::postgres_connection(port),
            ServerKind::Mysql => strings::mysql_connection(port),
            ServerKind::Valkey => strings::valkey_connection(port),
        };
        text.push('\n');
        text.push_str(&lines);
        text.push('\n');
    }
    text
}

/// One row per service, with a title row. Each column is as wide as its
/// widest cell, and two spaces apart.
fn table(services: &[ServiceStatus]) -> String {
    let mut rows = vec![strings::SERVICES_HEADER.map(str::to_owned)];
    rows.extend(services.iter().map(|status| {
        [
            status.id.to_string(),
            strings::state_name(&status.state).to_owned(),
            status
                .pid
                .map_or_else(|| strings::NO_PID.to_owned(), |pid| pid.to_string()),
            status.restarts.to_string(),
        ]
    }));
    let mut widths = [0; 4];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut text = String::new();
    for row in &rows {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(widths) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::data_dir::DataDir;

    fn status(
        id: ServiceId,
        state: ServiceState,
        pid: Option<u32>,
        restarts: u32,
    ) -> ServiceStatus {
        ServiceStatus {
            id,
            state,
            pid,
            restarts,
            since_ms: None,
        }
    }

    #[test]
    fn the_table_has_a_column_per_field() {
        let services = [
            status(ServiceId::Caddy, ServiceState::Running, Some(4242), 0),
            status(
                ServiceId::Database(ServerKind::Valkey),
                ServiceState::Stopped,
                None,
                12,
            ),
        ];
        assert_eq!(
            render(&services, Path::new("/data")),
            "SERVICE  STATE    PID   RESTARTS\n\
             caddy    running  4242  0\n\
             valkey   stopped  -     12\n"
        );
    }

    #[test]
    fn a_failed_service_says_why_under_the_table() {
        let failed = ServiceState::Failed {
            code: "port_in_use".into(),
        };
        let text = render(
            &[status(ServiceId::Caddy, failed, None, 3)],
            Path::new("/data"),
        );
        assert_eq!(
            text,
            "SERVICE  STATE   PID  RESTARTS\n\
             caddy    failed  -    3\n\
             caddy failed: A port Caddy needs is used by another program. Run \
             \"marsa logs caddy\" to see which one. [port_in_use]\n"
        );
    }

    /// While PostgreSQL runs, the table says how Laravel connects, on the
    /// port of `global.toml`: five lines to copy as they are.
    #[test]
    fn a_running_postgres_shows_how_to_connect() {
        let data = tempfile::tempdir().unwrap();
        let running = [status(
            ServiceId::Database(ServerKind::Postgres),
            ServiceState::Running,
            Some(7),
            0,
        )];
        assert_eq!(
            render(&running, data.path()),
            "SERVICE   STATE    PID  RESTARTS\n\
             postgres  running  7    0\n\
             \n\
             postgres: 127.0.0.1:5432, user root, no password. For a Laravel .env:\n\
             DB_CONNECTION=pgsql\n\
             DB_HOST=127.0.0.1\n\
             DB_PORT=5432\n\
             DB_USERNAME=root\n\
             DB_PASSWORD=\n"
        );
        std::fs::create_dir_all(DataDir::new(data.path()).config()).unwrap();
        let global = ConfigPaths::in_dir(data.path()).global;
        std::fs::write(&global, "[postgres]\nport = 5433\n").unwrap();
        let text = render(&running, data.path());
        assert!(text.contains("127.0.0.1:5433, user root"), "{text}");
        assert!(text.contains("\nDB_PORT=5433\n"), "{text}");
        let stopped = [status(
            ServiceId::Database(ServerKind::Postgres),
            ServiceState::Stopped,
            None,
            0,
        )];
        assert!(!render(&stopped, data.path()).contains("DB_"));
    }

    /// `marsa services start mysql` prints the five lines too.
    #[test]
    fn a_running_mysql_shows_how_to_connect() {
        let data = tempfile::tempdir().unwrap();
        let running = [status(
            ServiceId::Database(ServerKind::Mysql),
            ServiceState::Running,
            Some(7),
            0,
        )];
        let text = render(&running, data.path());
        assert!(
            text.ends_with(
                "\n\nmysql: 127.0.0.1:3306, user root, no password. For a Laravel .env:\n\
                 DB_CONNECTION=mysql\n\
                 DB_HOST=127.0.0.1\n\
                 DB_PORT=3306\n\
                 DB_USERNAME=root\n\
                 DB_PASSWORD=\n"
            ),
            "{text}"
        );
    }

    /// While Valkey runs, the table says how Laravel's Redis cache and
    /// queue reach it, on the port of `global.toml`.
    #[test]
    fn a_running_valkey_shows_how_to_connect() {
        let data = tempfile::tempdir().unwrap();
        let running = [status(
            ServiceId::Database(ServerKind::Valkey),
            ServiceState::Running,
            Some(7),
            0,
        )];
        let text = render(&running, data.path());
        assert!(
            text.ends_with(
                "\n\nvalkey: 127.0.0.1:6379, no password. For a Laravel .env:\n\
                 REDIS_HOST=127.0.0.1\n\
                 REDIS_PORT=6379\n"
            ),
            "{text}"
        );
        std::fs::create_dir_all(DataDir::new(data.path()).config()).unwrap();
        let global = ConfigPaths::in_dir(data.path()).global;
        std::fs::write(&global, "[valkey]\nport = 6380\n").unwrap();
        let text = render(&running, data.path());
        assert!(
            text.contains("valkey: 127.0.0.1:6380, no password"),
            "{text}"
        );
        assert!(text.ends_with("\nREDIS_PORT=6380\n"), "{text}");
        let stopped = [status(
            ServiceId::Database(ServerKind::Valkey),
            ServiceState::Stopped,
            None,
            0,
        )];
        assert!(!render(&stopped, data.path()).contains("REDIS_PORT"));
    }

    /// A failure stays under its row, and each running server's lines come
    /// after it, in the order of the servers.
    #[test]
    fn failures_come_first_then_each_server_in_its_own_block() {
        let data = tempfile::tempdir().unwrap();
        let failed = ServiceState::Failed {
            code: "port_in_use".into(),
        };
        let services = [
            status(ServiceId::Caddy, failed, None, 3),
            status(
                ServiceId::Database(ServerKind::Valkey),
                ServiceState::Running,
                Some(8),
                0,
            ),
            status(
                ServiceId::Database(ServerKind::Postgres),
                ServiceState::Running,
                Some(7),
                0,
            ),
        ];
        let text = render(&services, data.path());
        let blocks: Vec<&str> = text.split("\n\n").collect();
        assert_eq!(blocks.len(), 3, "{text}");
        assert!(blocks[0].ends_with("[port_in_use]"), "{text}");
        assert!(blocks[1].starts_with("postgres: "), "{text}");
        assert!(blocks[2].starts_with("valkey: "), "{text}");
    }

    #[test]
    fn no_services_is_a_sentence() {
        assert_eq!(
            render(&[], Path::new("/data")),
            "The daemon manages no services.\n"
        );
    }

    #[test]
    fn each_command_gives_its_action() {
        let id = || "caddy".to_owned();
        for (command, action) in [
            (ServicesCommand::Start { id: id() }, ServiceAction::Start),
            (ServicesCommand::Stop { id: id() }, ServiceAction::Stop),
            (
                ServicesCommand::Restart { id: id() },
                ServiceAction::Restart,
            ),
        ] {
            assert_eq!(command.action(), (action, "caddy"));
        }
    }
}
