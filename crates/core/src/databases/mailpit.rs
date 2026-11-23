//! Mailpit as a managed service: the mail catcher. Every mail a project
//! sends over SMTP stays in Mailpit's own database, in the app data folder,
//! and never leaves the computer.
//!
//! Not a database server, but it runs like one: an installed program, a
//! port that another program may hold, a table in `global.toml`, and a stop
//! by signal. So it is a [`super::ServerKind`] and reuses their manager.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::arg_path;
use crate::services::{Signal, StopMethod};

/// Mailpit's SMTP port when `[mailpit] port` is not set in `global.toml`:
/// the one Laravel's `.env.example` names (`MAIL_PORT=2525`), so a project
/// needs only `MAIL_MAILER=smtp`.
pub const MAILPIT_PORT: u16 = 2525;

/// The port of Mailpit's web page and API. Fixed: only `marsad` and the
/// browser use it, and the daemon checks it before each start too.
pub const MAILPIT_UI_PORT: u16 = 8025;

/// The prefix of the environment variables that Mailpit reads as settings.
/// `marsad` passes none of them on ([`mailpit_env_remove`]): one such as
/// `MP_SMTP_RELAY_CONFIG` would send the caught mail out.
pub const ENV_PREFIX: &str = "MP_";

/// How long Mailpit has to exit after SIGTERM. It closes its database and
/// exits at once.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// `<app data>/mail/mailpit.db`: Mailpit's SQLite database. One for every
/// version; removing a version never deletes it.
pub fn mailpit_database(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("mail").join("mailpit.db")
}

/// The arguments of `mailpit`, for `smtp_port` and the database `database`.
///
/// - Both listeners on `127.0.0.1` only.
/// - `--disable-version-check`: no request to GitHub.
/// - `--smtp-auth-accept-any`: a project's `.env` may set
///   `MAIL_USERNAME` and `MAIL_PASSWORD`; Mailpit takes any, or none.
/// - `--smtp-disable-rdns`: no DNS lookup for each connection.
///
/// Never a relay, a forward, a webhook, or a spam checker: nothing that
/// sends a mail or a request off the computer.
pub fn mailpit_args(smtp_port: u16, database: &Path) -> Vec<String> {
    vec![
        "--smtp".to_owned(),
        format!("127.0.0.1:{smtp_port}"),
        "--listen".to_owned(),
        format!("127.0.0.1:{MAILPIT_UI_PORT}"),
        "--database".to_owned(),
        arg_path(database),
        "--disable-version-check".to_owned(),
        "--smtp-auth-accept-any".to_owned(),
        "--smtp-disable-rdns".to_owned(),
    ]
}

/// The names in `names` that Mailpit would read as settings: those that
/// start with [`ENV_PREFIX`]. `names` is the daemon's environment.
pub fn mailpit_env_remove<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .filter(|name| name.as_ref().starts_with(ENV_PREFIX))
        .map(|name| name.as_ref().to_owned())
        .collect()
}

/// SIGTERM, then a kill. Windows has no signals: there, the kill.
pub fn mailpit_stop() -> StopMethod {
    StopMethod::signals([(Signal::Term, STOP_WAIT)])
}

/// Whether `banner`, the first bytes Mailpit sends on its SMTP port, is the
/// greeting of a server ready for mail: `220 `.
pub fn is_smtp_greeting(banner: &[u8]) -> bool {
    banner.starts_with(b"220 ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_bind_loopback_and_never_relay() {
        let database = Path::new("/data/mail/mailpit.db");
        let args = mailpit_args(2526, database);
        assert_eq!(
            args,
            [
                "--smtp",
                "127.0.0.1:2526",
                "--listen",
                "127.0.0.1:8025",
                "--database",
                "/data/mail/mailpit.db",
                "--disable-version-check",
                "--smtp-auth-accept-any",
                "--smtp-disable-rdns",
            ]
        );
        for arg in &args {
            for refused in ["relay", "forward", "webhook", "spamassassin", "rspamd"] {
                assert!(!arg.contains(refused), "{arg}");
            }
        }
    }

    #[test]
    fn database_is_in_the_mail_folder() {
        assert_eq!(
            mailpit_database(Path::new("/data")),
            Path::new("/data").join("mail").join("mailpit.db")
        );
    }

    #[test]
    fn only_mailpit_settings_are_removed() {
        let names = [
            "PATH",
            "MP_SMTP_RELAY_CONFIG",
            "MP_WEBHOOK_URL",
            "HOME",
            "XMP_X",
        ];
        assert_eq!(
            mailpit_env_remove(names),
            ["MP_SMTP_RELAY_CONFIG", "MP_WEBHOOK_URL"]
        );
    }

    #[test]
    fn the_smtp_greeting_is_220_and_a_space() {
        assert!(is_smtp_greeting(b"220 localhost Mailpit ESMTP"));
        for bad in [&b"220-"[..], b"421 ", b"22", b""] {
            assert!(!is_smtp_greeting(bad), "{bad:?}");
        }
    }
}
