//! The dumps `marsad` caught from PHP: `GET /v1/dumps`. The window and the
//! CLI never talk to PHP. Everything here is text: nothing is HTML.

use serde::{Deserialize, Serialize};

/// One `dump()` call, as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dump {
    /// Grows with every dump the daemon keeps, and is never used twice, also
    /// after a clear. `0` until the daemon gives it.
    pub id: u64,
    /// In milliseconds since the Unix epoch.
    pub received_at: u64,
    /// The PHP file that called `dump()`.
    pub source_file: Option<String>,
    pub source_line: Option<u32>,
    /// The URL of the request, for a dump made while serving a web request.
    pub uri: Option<String>,
    pub method: Option<String>,
    /// What the second argument of `VarDumper::dump` said.
    pub label: Option<String>,
    /// The value, written as Symfony's `CliDumper` writes it, without colors.
    pub text: String,
    /// The text was cut at its limit (see `dumps::MAX_TEXT_BYTES`).
    pub truncated: bool,
}

/// What the dump server is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DumpsStatus {
    /// The listener is up, so web dumps arrive here.
    Listening,
    /// Another program holds the port (for example `php artisan dump-server`).
    PortInUse,
    /// `[dumps] enabled = false`.
    #[default]
    Disabled,
}

/// `GET /v1/dumps`, and the answer to `DELETE /v1/dumps`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DumpList {
    pub status: DumpsStatus,
    /// The `id` of the newest dump ever kept, also after a clear. A client
    /// asks for `after` this to get only the new ones.
    pub latest_id: u64,
    /// Newest first.
    pub dumps: Vec<Dump>,
}

#[cfg(test)]
mod dump_tests {
    use super::*;

    #[test]
    fn a_list_round_trips_and_the_status_is_a_word() {
        let list = DumpList {
            status: DumpsStatus::PortInUse,
            latest_id: 9,
            dumps: vec![Dump {
                id: 9,
                received_at: 1_700_000_000_000,
                source_file: Some("/app/routes/web.php".into()),
                source_line: Some(12),
                uri: None,
                method: None,
                label: None,
                text: "\"hi\"".into(),
                truncated: false,
            }],
        };
        let value = serde_json::to_value(&list).expect("json");
        assert_eq!(value["status"], "port_in_use");
        assert_eq!(
            serde_json::from_value::<DumpList>(value).expect("back"),
            list
        );
    }
}
