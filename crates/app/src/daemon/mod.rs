//! The daemon: API server, auth, process supervisor, and Caddy manager
//! (architecture.md section 5). One file per job.

pub mod api;
pub mod auth;
pub mod autostart;
pub(crate) mod http;
pub mod runtimes;
pub mod server;
pub mod services;
pub mod setup;
pub mod sites;
pub mod state;
#[cfg(test)]
pub(crate) mod test_router;
