//! What every managed service (Caddy, php-fpm, a database server) shares:
//! the process probe, the action queue, and the runtime bookkeeping.

pub mod actions;
pub mod api;
pub mod caddy;
pub mod databases;
pub mod dns;
pub mod php;
pub mod pid_record;
pub mod probe;
pub mod runtime;
pub mod supervisor;
