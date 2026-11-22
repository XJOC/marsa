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
pub mod doctor_api;
pub mod hosts_sync;
pub(crate) mod http;
pub mod one_at_a_time;
pub mod php;
pub mod runtimes;
pub mod server;
pub mod services;
pub mod services_api;
pub mod setup_api;
pub mod sites;
pub mod sites_api;
pub mod sites_sync;
pub mod state;
pub mod supervisor;
#[cfg(test)]
pub(crate) mod test_router;
pub mod uninstall_api;
pub mod watch;
pub mod xdebug_api;
