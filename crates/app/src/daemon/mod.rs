//! The daemon: API server, auth, process supervisor, and Caddy manager
//! (architecture.md section 5). One file per job.

pub mod api;
pub mod auth;
pub mod autostart;
pub mod autostart_api;
pub mod caddy;
pub mod client_shims;
pub mod databases;
pub mod dns;
pub mod hosts_sync;
pub(crate) mod http;
pub mod php;
pub mod runtimes;
pub mod runtimes_api;
pub mod server;
pub mod services;
pub mod services_api;
pub mod setup_api;
pub mod sites_api;
pub mod sites_sync;
pub mod state;
pub mod supervisor;
pub mod uninstall_api;
pub mod watch;
pub mod xdebug_api;
