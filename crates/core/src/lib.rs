//! `marsa-core`: all of Marsa's logic, split into modules.
//!
//! This is a functional core: functions take data and return data or a plan.
//! They do not start processes or open sockets, except in the three modules
//! behind the `full` feature: `platform`, `installer`, and `client`.

pub mod caddy;
pub mod codes;
pub mod config;
pub mod data_dir;
pub mod databases;
pub mod dns;
pub mod doctor;
pub mod fs;
pub mod helper;
pub mod hex;
pub mod orphan;
pub mod php;
pub mod ports;
pub mod ps;
pub mod recipes;
pub mod resolver;
pub mod runtimes;
pub mod services;
pub mod sites;
pub mod types;

#[cfg(feature = "full")]
pub mod client;
#[cfg(feature = "full")]
pub mod installer;
#[cfg(feature = "full")]
pub mod platform;
// `full` only because the Windows plan asks the helper's certificate parser
// (`helper::certificate_sha256`), which needs `sha2`. The shim never plans a setup.
#[cfg(feature = "full")]
pub mod setup;
