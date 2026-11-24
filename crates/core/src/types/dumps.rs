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

/// The most dumps the daemon keeps, and the window with it.
pub const MAX_KEPT_DUMPS: usize = 500;
/// The most text (by [`Dump::weight`]) the daemon keeps, and the window with
/// it.
pub const MAX_KEPT_BYTES: usize = 16 * 1024 * 1024;

impl Dump {
    /// What this dump takes in memory, near enough: its strings and a fixed
    /// part. The daemon and the window both keep dumps under
    /// [`MAX_KEPT_BYTES`] by it.
    pub fn weight(&self) -> usize {
        let strings = [&self.source_file, &self.uri, &self.method, &self.label];
        let optional: usize = strings
            .iter()
            .map(|text| text.as_ref().map_or(0, String::len))
            .sum();
        self.text.len() + optional + 128
    }
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
    /// How many dumps the daemon holds now, also those that `after` left
    /// out. A client that keeps the dumps it read drops its oldest ones
    /// until it holds no more than this: the daemon has cleared or dropped
    /// them.
    pub count: u64,
    /// Newest first.
    pub dumps: Vec<Dump>,
}

/// `GET` and `PUT /v1/dumps/capture`: whether `dump()` goes to the window.
/// The setting is `[dumps] enabled` in `global.toml`; whether the port is
/// free is [`DumpList::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DumpsCapture {
    pub enabled: bool,
}

#[cfg(test)]
mod dump_tests {
    use super::*;

    #[test]
    fn the_capture_body_is_one_flag() {
        let on = DumpsCapture { enabled: true };
        assert_eq!(
            serde_json::to_value(on).expect("json"),
            serde_json::json!({ "enabled": true })
        );
        assert!(serde_json::from_value::<DumpsCapture>(serde_json::json!({})).is_err());
    }

    #[test]
    fn a_list_round_trips_and_the_status_is_a_word() {
        let list = DumpList {
            status: DumpsStatus::PortInUse,
            latest_id: 9,
            count: 1,
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
