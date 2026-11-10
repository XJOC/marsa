//! Paths, process spawning, port checks, and service registration. Platform
//! code lives here, including `platform::windows`.
//!
//! One file per M3 task, so the tasks can run in parallel (task M3-01).

pub mod autostart;
pub mod daemon_file;
pub mod process;
pub mod signals;
pub mod spawn;
