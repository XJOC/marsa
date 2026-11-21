//! What every managed service (Caddy, php-fpm, a database server) shares:
//! the process probe, the action queue, and the runtime bookkeeping.

pub mod probe;
pub mod runtime;
