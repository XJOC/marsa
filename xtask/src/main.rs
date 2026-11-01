//! `xtask`: repository chores that are easier in Rust than in a shell script.
//!
//! Run it as `cargo xtask <command>`; the alias lives in `.cargo/config.toml`.

use clap::Command;

fn main() {
    Command::new("xtask")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Repository tasks for the Marsa workspace")
        .get_matches();
}
