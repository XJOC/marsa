//! The database servers of `marsad` (ADR 0023), on macOS, Linux, and
//! Windows: PostgreSQL, MySQL, and Valkey, and the mail catcher Mailpit. `marsa_core::databases` decides;
//! this module runs the programs.
//!
//! The first start of a line makes its data folder (`init::ensure_data`):
//! for PostgreSQL its cluster in `databases/postgres/<major>/`
//! (`postgres::ensure_cluster`). All of it happens in a temp folder next to
//! the target, which is renamed into place at the end, so a data folder that
//! is half made is never taken for a ready one.
//!
//! Before each start, `facts::start_facts` reads the port and the
//! server's own pid file (`postmaster.pid`) from the system for
//! `decide_start`. It only reads: nothing here signals a process or deletes
//! that file.
//!
//! [`ServerManager`] starts and stops one server under the supervisor, one
//! action at a time. A failed start leaves the status `failed`, with the
//! reason in the recent lines. A run that ends by itself before it takes
//! connections is read once: if PostgreSQL refused because a live process
//! holds one of its lock files, the start fails as the check before it
//! would have. Nothing reads the log after the server is ready.
//!
//! [`start_at_launch`] starts a server with `marsad` when its table in
//! `global.toml` says `enabled = true` (ADR 0023: a server that is on
//! starts with the daemon). A failure leaves the daemon running and the
//! status `failed`.

pub mod client_shims;
mod facts;
mod init;
mod mailpit;
mod manager;
mod mysql;
mod orphan;
mod postgres;
mod settings;
mod valkey;

pub use init::InitError;
pub use manager::{ActionError, ServerManager};
pub use settings::{start_after_install, start_at_launch};

#[cfg(test)]
mod tests;
