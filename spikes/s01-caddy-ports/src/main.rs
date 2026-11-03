//! S-01 port probe. `std` only.
//!
//! * `bind <addr:port>...`  bind every address, keep them open together, close.
//! * `hold <addr:port> [--seconds N]`  bind and stay (the "other program").
//! * `connect <addr:port>`  connect with a one-second timeout.
//!
//! `TcpListener::bind` also calls `listen()` and, on Unix, sets
//! `SO_REUSEADDR`. Nothing here ever calls `accept`.

use std::env;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

const USAGE: &str =
    "usage: port-probe bind <addr:port>... | hold <addr:port> [--seconds N] | connect <addr:port>";
const DEFAULT_HOLD_SECONDS: u64 = 30;
const MAX_HOLD_SECONDS: u64 = 300;

/// `kind`, raw errno, and message. EACCES is 13, EADDRINUSE is 48 on macOS.
fn describe(err: &io::Error) -> String {
    let errno = err
        .raw_os_error()
        .map_or_else(|| "none".to_string(), |code| code.to_string());
    format!("kind={:?} raw_os_error={errno} message={err}", err.kind())
}

fn parse_addr(text: &str) -> Result<SocketAddr, String> {
    text.parse::<SocketAddr>()
        .map_err(|err| format!("bad address {text:?}: {err}"))
}

fn cmd_bind(args: &[String]) -> Result<bool, String> {
    if args.is_empty() {
        return Err("bind needs at least one address".to_string());
    }
    let mut open = Vec::new();
    let mut all_ok = true;
    for text in args {
        let addr = parse_addr(text)?;
        match TcpListener::bind(addr) {
            Ok(listener) => {
                println!("OK bind {addr}");
                open.push(listener);
            }
            Err(err) => {
                println!("FAIL bind {addr} {}", describe(&err));
                all_ok = false;
            }
        }
    }
    drop(open);
    Ok(all_ok)
}

fn cmd_hold(args: &[String]) -> Result<bool, String> {
    let (text, seconds) = match args {
        [addr] => (addr, DEFAULT_HOLD_SECONDS),
        [addr, flag, n] if flag == "--seconds" => {
            let n = n
                .parse::<u64>()
                .map_err(|err| format!("bad seconds {n:?}: {err}"))?;
            (addr, n.min(MAX_HOLD_SECONDS))
        }
        _ => return Err("hold needs <addr:port> [--seconds N]".to_string()),
    };
    let addr = parse_addr(text)?;
    match TcpListener::bind(addr) {
        Ok(listener) => {
            println!(
                "OK holding {addr} for {seconds}s pid={}",
                std::process::id()
            );
            thread::sleep(Duration::from_secs(seconds));
            drop(listener);
            println!("released {addr}");
            Ok(true)
        }
        Err(err) => {
            println!("FAIL hold {addr} {}", describe(&err));
            Ok(false)
        }
    }
}

fn cmd_connect(args: &[String]) -> Result<bool, String> {
    let [text] = args else {
        return Err("connect needs <addr:port>".to_string());
    };
    let addr = parse_addr(text)?;
    match TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
        Ok(_) => {
            println!("CONNECTED {addr}");
            Ok(true)
        }
        Err(err) if err.kind() == ErrorKind::ConnectionRefused => {
            println!("REFUSED {addr} {}", describe(&err));
            Ok(false)
        }
        Err(err) if err.kind() == ErrorKind::TimedOut => {
            println!("TIMEOUT {addr} {}", describe(&err));
            Ok(false)
        }
        Err(err) => {
            println!("ERROR {addr} {}", describe(&err));
            Ok(false)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((cmd, rest)) if cmd == "bind" => cmd_bind(rest),
        Some((cmd, rest)) if cmd == "hold" => cmd_hold(rest),
        Some((cmd, rest)) if cmd == "connect" => cmd_connect(rest),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}
