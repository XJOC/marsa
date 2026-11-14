//! The daemon: API server, auth, process supervisor, and Caddy manager
//! (architecture.md section 5). One file per M3 task, so the tasks can run in
//! parallel (task M3-01).

pub mod api;
pub mod auth;
pub mod autostart_api;
pub mod caddy;
pub mod dns;
pub mod php;
pub mod runtimes_api;
pub mod server;
pub mod services_api;
pub mod setup_api;
pub mod sites_api;
pub mod sites_sync;
pub mod state;
pub mod supervisor;
pub mod uninstall_api;
pub mod watch;
