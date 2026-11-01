//! `marsa`: the command-line client. It never changes state by itself; every
//! command maps to a route on the daemon.

use clap::Command;

fn main() {
    Command::new("marsa")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Command-line client for the Marsa daemon")
        .get_matches();
}
