//! The daemon: API server, auth, process supervisor, and Caddy manager
//! (architecture.md section 5). One file per job.

pub mod api;
pub mod auth;
pub mod autostart;
pub mod autostart_api;
pub mod doctor_api;
pub(crate) mod http;
pub mod one_at_a_time;
pub mod runtimes;
pub mod server;
pub mod services;
pub mod setup_api;
pub mod sites;
pub mod state;
#[cfg(test)]
pub(crate) mod test_router;
pub mod uninstall_api;
pub mod xdebug_api;
