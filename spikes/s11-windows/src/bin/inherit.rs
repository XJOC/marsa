//! Does a detached child keep the caller's stdout pipe open?
//!
//! ```text
//! inherit parent [--fix]   prints one line, starts `inherit child` detached, exits
//! inherit child            sleeps 20 seconds
//! ```
//!
//! The parent starts the child the way `spawn_marsad` starts `marsad`: null
//! stdin, stdout, and stderr, `DETACHED_PROCESS | CREATE_NO_WINDOW`. std calls
//! `CreateProcessW` with `bInheritHandles = TRUE`, so every inheritable handle
//! of the parent reaches the child. In `inherit parent | findstr x`, the
//! parent's stdout is an inheritable pipe. If the child holds a copy, the
//! reader waits until the child exits. `--fix` clears the inherit flag on the
//! parent's three standard handles first.

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("parent") => {
            println!("started");
            if args.iter().any(|arg| arg == "--fix") {
                for result in s11_windows::stop_std_handles_inheritance() {
                    result?;
                }
            }
            Command::new(std::env::current_exe()?)
                .arg("child")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
                .spawn()?;
            Ok(())
        }
        Some("child") => {
            std::thread::sleep(std::time::Duration::from_secs(20));
            Ok(())
        }
        _ => Err(std::io::Error::other(
            "usage: inherit parent [--fix] | child",
        )),
    }
}

#[cfg(not(windows))]
fn main() {}
