//! `marsa-helper`: the privileged helper.
//!
//! It accepts only typed JSON requests from a closed list of admin operations
//! and never runs a command string it receives. It does not depend on
//! `marsa-core` (architecture.md section 7).

fn main() -> std::process::ExitCode {
    std::process::ExitCode::SUCCESS
}
