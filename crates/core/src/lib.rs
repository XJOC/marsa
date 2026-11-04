//! `marsa-core`: all of Marsa's logic, split into modules.
//!
//! This is a functional core: functions take data and return data or a plan.
//! They do not start processes or open sockets, except in the three modules
//! behind the `full` feature: `platform`, `installer`, and `client`.

pub mod caddy;
pub mod config;
pub mod dns;
pub mod helper;
pub mod orphan;
pub mod php;
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
