//! Helpers the integration tests share. Each test binary uses some of them.
#![allow(dead_code, reason = "each test binary uses only part of this module")]

use std::net::{Ipv4Addr, TcpListener};

use marsa_core::types::ServiceState;

/// A port that was free a moment ago, on `127.0.0.1`.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
    listener.local_addr().expect("an address").port()
}

/// The state of a service that failed with `code`.
pub fn failed(code: &str) -> ServiceState {
    ServiceState::Failed { code: code.into() }
}

/// A program's output as text, with Windows line ends turned into `\n`.
pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}
