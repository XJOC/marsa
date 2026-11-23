//! What only Mailpit needs: its mail folder, how it runs, and how it says
//! it is ready.
//!
//! Mailpit has no first init: it makes its SQLite database at its first
//! start. It writes no pid file and takes no lock that another start would
//! meet, so the check before a start is its two ports and our own record of
//! its pid (`run/mailpit.pid`).

use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marsa_core::databases::ServerKind;
use marsa_core::databases::mailpit::{
    is_smtp_greeting, mailpit_args, mailpit_env_remove, mailpit_stop,
};
use marsa_core::platform::daemon_file::create_private_dir;
use marsa_core::services::{RestartPolicy, ServiceSpec};
use marsa_core::types::ServiceId;
use tokio::io::AsyncReadExt as _;

/// For each attempt of the readiness check.
const CONNECT_LIMIT: Duration = Duration::from_millis(500);

/// How the supervisor runs Mailpit: SMTP on `127.0.0.1:<smtp_port>`, its
/// web page on `127.0.0.1:8025`, and its database at `database`
/// ([`mailpit_args`]). The daemon's `MP_*` variables are taken out of its
/// environment ([`mailpit_env_remove`]): Mailpit reads them as settings, and
/// one could relay the caught mail to a real server. Stopped with SIGTERM,
/// then a kill.
///
/// Never restarted after it exits, as the database servers: a refusal at
/// its start would only come again, and the status must keep the run's own
/// exit.
pub(super) fn spec(program: PathBuf, smtp_port: u16, database: &Path) -> ServiceSpec {
    let names = std::env::vars_os().filter_map(|(name, _)| name.into_string().ok());
    ServiceSpec {
        id: ServiceId::Database(ServerKind::Mailpit),
        program,
        args: mailpit_args(smtp_port, database),
        env: Default::default(),
        env_remove: mailpit_env_remove(names),
        cwd: None,
        restart: RestartPolicy::Never,
        stop: mailpit_stop(),
    }
}

/// Makes the folder of `database` owner-only: the caught mail can hold
/// password-reset links and personal data. Mailpit makes the file itself.
pub(super) async fn make_folder(database: &Path) -> io::Result<()> {
    let Some(folder) = database.parent().map(Path::to_path_buf) else {
        return Err(io::Error::other("the mail database has no folder"));
    };
    let job = tokio::task::spawn_blocking(move || create_private_dir(&folder));
    job.await.map_err(io::Error::other)?
}

/// Whether Mailpit takes mail on `127.0.0.1:<port>`: it greets a new SMTP
/// connection with `220 ` ([`is_smtp_greeting`]).
pub(super) async fn answers(port: u16) -> bool {
    let ask = async {
        let address = (Ipv4Addr::LOCALHOST, port);
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        let mut banner = [0_u8; 4];
        stream.read_exact(&mut banner).await.ok()?;
        Some(is_smtp_greeting(&banner))
    };
    matches!(
        tokio::time::timeout(CONNECT_LIMIT, ask).await,
        Ok(Some(true))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_runs_mailpit_on_loopback_without_mp_variables() {
        let database = Path::new("/data/mail/mailpit.db");
        let spec = spec(PathBuf::from("/bin/mailpit"), 2525, database);
        assert_eq!(spec.id.to_string(), "mailpit");
        assert!(spec.args.contains(&"127.0.0.1:2525".to_owned()));
        assert!(spec.args.contains(&"127.0.0.1:8025".to_owned()));
        assert!(spec.env_remove.iter().all(|name| name.starts_with("MP_")));
        assert_eq!(spec.restart, RestartPolicy::Never);
    }

    #[tokio::test]
    async fn a_greeting_of_220_is_ready_and_silence_is_not() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            let (mut stream, _) = listener.accept().await.expect("accept");
            stream
                .write_all(b"220 localhost Mailpit ESMTP\r\n")
                .await
                .expect("write");
        });
        assert!(answers(port).await);
        server.await.expect("server");

        let silent = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let port = silent.local_addr().expect("address").port();
        assert!(!answers(port).await);
    }
}
