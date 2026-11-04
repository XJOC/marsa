//! `marsa services`: the table of the services the daemon manages, and
//! `marsa services start|stop|restart <id>`. Task M3-09.

use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::DaemonClient;
use marsa_core::types::{ServiceAction, ServiceState, ServiceStatus};

use super::daemon::{ClientFailure, connect, failure};
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
pub async fn run(args: &ServicesArgs, data_dir: &Path) -> ExitCode {
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    match &args.command {
        None => list(&client, data_dir).await,
        Some(command) => act(&client, command, data_dir).await,
    }
}

async fn list(client: &DaemonClient, data_dir: &Path) -> ExitCode {
    match client.services().await {
        Ok(services) => {
            print!("{}", render(&services, data_dir));
            ExitCode::SUCCESS
        }
        Err(err) => failure(&err, data_dir, None).report(),
    }
}

async fn act(client: &DaemonClient, command: &ServicesCommand, data_dir: &Path) -> ExitCode {
    let (action, id) = command.action();
    let status = match client.service_action(id, action).await {
        Ok(status) => status,
        Err(err) => return failure(&err, data_dir, Some(id)).report(),
    };
    // After a stop the service is not running, which is what was asked; a
    // failure from before is shown under the table.
    let failed = match (&status.state, action) {
        (ServiceState::Failed { code }, ServiceAction::Start | ServiceAction::Restart) => {
            Some(ClientFailure::new(code, data_dir, Some(&status.id)))
        }
        _ => None,
    };
    match failed {
        Some(failure) => {
            print!("{}", table(std::slice::from_ref(&status)));
            failure.report()
        }
        None => {
            print!("{}", render(std::slice::from_ref(&status), data_dir));
            ExitCode::SUCCESS
        }
    }
}

/// The table, then one line per failed service that says why it failed.
pub fn render(services: &[ServiceStatus], data_dir: &Path) -> String {
    if services.is_empty() {
        return format!("{}\n", strings::NO_SERVICES);
    }
    let mut text = table(services);
    for status in services {
        if let ServiceState::Failed { code } = &status.state {
            let failure = ClientFailure::new(code, data_dir, Some(&status.id));
            text.push_str(&strings::service_failed(
                &status.id,
                &failure.message,
                &failure.code,
            ));
            text.push('\n');
        }
    }
    text
}

/// One row per service, with a title row. Each column is as wide as its
/// widest cell, and two spaces apart.
fn table(services: &[ServiceStatus]) -> String {
    let mut rows = vec![strings::SERVICES_HEADER.map(str::to_owned)];
    rows.extend(services.iter().map(|status| {
        [
            status.id.clone(),
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

    fn status(id: &str, state: ServiceState, pid: Option<u32>, restarts: u32) -> ServiceStatus {
        ServiceStatus {
            id: id.into(),
            state,
            pid,
            restarts,
            since_ms: None,
        }
    }

    #[test]
    fn the_table_has_a_column_per_field() {
        let services = [
            status("caddy", ServiceState::Running, Some(4242), 0),
            status("php-8.4", ServiceState::Stopped, None, 12),
        ];
        assert_eq!(
            render(&services, Path::new("/data")),
            "SERVICE  STATE    PID   RESTARTS\n\
             caddy    running  4242  0\n\
             php-8.4  stopped  -     12\n"
        );
    }

    #[test]
    fn a_failed_service_says_why_under_the_table() {
        let failed = ServiceState::Failed {
            code: "port_in_use".into(),
        };
        let text = render(&[status("caddy", failed, None, 3)], Path::new("/data"));
        assert_eq!(
            text,
            "SERVICE  STATE   PID  RESTARTS\n\
             caddy    failed  -    3\n\
             caddy failed: A port Caddy needs is used by another program. Run \
             \"marsa logs caddy\" to see which one. [port_in_use]\n"
        );
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
