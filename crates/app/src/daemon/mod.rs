//! The daemon: API server, auth, process supervisor, and Caddy manager
//! (architecture.md section 5). One file per M3 task, so the tasks can run in
//! parallel (task M3-01).

pub mod api;
pub mod auth;
pub mod caddy;
pub mod dns;
pub mod runtimes_api;
pub mod server;
pub mod services_api;
pub mod sites_api;
pub mod state;
pub mod supervisor;
pub mod watch;
