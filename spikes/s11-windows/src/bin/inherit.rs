//! Does a detached child keep the caller's stdout pipe open?
//!
//! ```text
//! inherit parent <marker> [--fix | --fix-all | --no-inherit]
//! inherit child <marker>     writes <marker>, prints, sleeps 20 seconds
//! ```
//!
//! The parent prints one line, starts the child detached, and exits. By
//! default it starts the child the way `spawn_marsad` does: null stdin,
//! stdout, and stderr, `DETACHED_PROCESS | CREATE_NO_WINDOW`. std calls
//! `CreateProcessW` with `bInheritHandles = TRUE`, so every inheritable handle
//! of the parent reaches the child. In `inherit parent | findstr x`, the
//! parent's stdout is an inheritable pipe. If the child holds a copy, the
//! reader waits until the child exits.
//!
//! - `--fix`: clear the inherit flag on the parent's three standard handles.
//! - `--fix-all`: clear it on every handle of the parent.
//! - `--no-inherit`: call `CreateProcessW` with `bInheritHandles = FALSE`.
//!
//! The child writes `<marker>` and then prints to stdout and stderr, to show
//! that it runs and that printing without standard handles does not stop it.

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["parent", marker, rest @ ..] => {
            println!("started");
            let exe = std::env::current_exe()?;
            if rest.contains(&"--no-inherit") {
                let exe = exe.to_string_lossy();
                s11_windows::spawn_without_handles(&exe, &["child", marker])?;
                return Ok(());
            }
            if rest.contains(&"--fix") {
                for result in s11_windows::stop_std_handles_inheritance() {
                    result?;
                }
            }
            if rest.contains(&"--fix-all") {
                s11_windows::stop_all_handles_inheritance();
            }
            Command::new(exe)
                .args(["child", marker])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
                .spawn()?;
            Ok(())
        }
        ["child", marker] => {
            std::fs::write(marker, "started")?;
            println!("child on stdout");
            eprintln!("child on stderr");
            std::fs::write(marker, "printed")?;
            std::thread::sleep(std::time::Duration::from_secs(20));
            Ok(())
        }
        _ => Err(std::io::Error::other(
            "usage: inherit parent <marker> [--fix | --fix-all | --no-inherit] | child <marker>",
        )),
    }
}

#[cfg(not(windows))]
fn main() {}
