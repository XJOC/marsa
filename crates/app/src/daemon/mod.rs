//! The daemon: API server, auth, process supervisor, and Caddy manager
//! (architecture.md section 5). One file per job.

pub mod autostart;
pub mod db_browser;
pub mod dumps;
pub mod http;
pub mod jobs;
pub mod log_filter;
pub mod mail;
pub mod runtimes;
pub mod services;
pub mod setup;
pub mod sites;
pub mod state;
pub mod workers;
