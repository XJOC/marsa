//! Every text the `marsa` command shows the user, in one place (AGENTS.md
//! section 9), so a second language can be added later without a rewrite.
//!
//! Errors: [`message`] maps an error `code()` to a sentence. The error's own
//! `Display` text goes to the log only. To add a code, add it to
//! `marsa_core::codes` and one arm to `known`: a test fails until both exist.

mod doctor;
mod errors;
mod mail;
mod runtimes;
mod services;
mod setup;
mod sites;

#[cfg(test)]
mod tests;

pub use doctor::*;
use errors::*;
pub use mail::*;
pub use runtimes::*;
pub use services::*;
pub use setup::*;
pub use sites::*;

use std::path::Path;

use marsa_core::runtimes::Runtime;

/// The facts a message may show. Fill in what the error has; a message whose
/// fact is missing reads without it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Details<'a> {
    pub runtime: Option<Runtime>,
    /// The version text the user asked for.
    pub requested: Option<&'a str>,
    /// Versions the user could ask for instead, or the parked folders for
    /// `folder_not_parked`.
    pub available: &'a [String],
    /// A platform key, such as `darwin-arm64`, or "this system".
    pub platform: Option<&'a str>,
    pub path: Option<&'a Path>,
    pub url: Option<&'a str>,
    /// The code of the error under this one, such as the last mirror's error
    /// under `download_failed`.
    pub cause: Option<&'a str>,
    /// What a program printed.
    pub output: Option<&'a str>,
    /// The system version needed, such as `macOS 15.0`.
    pub needs: Option<&'a str>,
    /// The system version found, such as `macOS 14.6`.
    pub found: Option<&'a str>,
    /// The id of the service the command is about, such as `caddy`.
    pub service: Option<&'a str>,
    /// The site name the command is about, such as `shop`, or the folder
    /// name a site name could not be made from.
    pub site: Option<&'a str>,
}

/// The sentence for `code`. An unknown code gets a general sentence; the
/// caller prints the code next to it.
pub fn message(code: &str, details: &Details<'_>) -> String {
    known(code, details).unwrap_or_else(|| {
        "Something went wrong. Run again with MARSA_LOG=debug for details.".to_owned()
    })
}

/// The name of a runtime in sentences.
pub fn runtime_name(runtime: Runtime) -> &'static str {
    match runtime {
        Runtime::Php => "PHP",
        Runtime::Node => "Node.js",
        Runtime::Composer => "Composer",
        Runtime::Caddy => "Caddy",
        Runtime::Postgres => "PostgreSQL",
        Runtime::Mysql => "MySQL",
        Runtime::Valkey => "Valkey",
        Runtime::Mailpit => "Mailpit",
    }
}

/// The name of a service in sentences: `Caddy` for `caddy`, else the id.
pub fn service_name(id: &str) -> &str {
    match id {
        "caddy" => "Caddy",
        "postgres" => "PostgreSQL",
        "mysql" => "MySQL",
        "valkey" => "Valkey",
        other => other,
    }
}
