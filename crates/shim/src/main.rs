//! `marsa-shim`: runs the real program at the version the project asks for.
//!
//! It is installed under many names (`php`, `node`, `npm`, `npx`, `composer`)
//! and learns its job from its own file name. It never talks to the daemon and
//! never uses tokio.

fn main() -> std::process::ExitCode {
    std::process::ExitCode::SUCCESS
}
