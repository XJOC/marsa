//! Platform code for the shim. `marsa_core::platform` sits behind the `full`
//! feature (tokio), which the shim must not pull in, so the shim keeps its own
//! small module. These are the only `#[cfg(unix)]` and `#[cfg(windows)]`
//! switches in the shim's non-test code.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::exec;

// A stub until task M1-07, so the shim builds and its tests run in Windows CI.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::exec;
