//! Job Objects and `CREATE_NO_WINDOW` with a tree of child processes.
//!
//! ```text
//! jobtree  parent <log> <depth> [--job] [--no-window]   console program
//! jobtreew parent <log> <depth> [--job] [--no-window]   windows subsystem, like marsad
//! jobtree  child  <log> <depth>
//! ```
//!
//! The parent optionally joins a kill-on-close job, starts `jobtree child`
//! (always the console program, like Caddy and php-cgi), and sleeps. Each
//! child starts the next one until `depth` is 1. Every process appends one
//! line to `<log>`: its pid, session, whether it is in a job, and whether it
//! has a console window and the window is visible. The script then kills the
//! parent and looks at which pids are still alive.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio, id};
use std::thread;
use std::time::Duration;

use crate::{console_window, current_session, in_job, join_kill_on_close_job};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let log = args.get(1).cloned().unwrap_or_default();
    let result = match args.first().map(String::as_str) {
        Some("parent") => parent(&args),
        Some("child") => child(&args),
        _ => Err(io::Error::other(
            "usage: jobtree parent|child <log> <depth>",
        )),
    };
    if let Err(err) = result {
        // The windows-subsystem parent has no console, so the log gets it.
        let _ = append(&log, &format!("role=error pid={} error={err}", id()));
        std::process::exit(1);
    }
}

fn append(log: &str, line: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(log)?;
    writeln!(file, "{line}")
}

fn depth_of(args: &[String]) -> io::Result<u32> {
    args.get(2)
        .and_then(|depth| depth.parse().ok())
        .ok_or_else(|| io::Error::other("depth is missing"))
}

fn describe(role: &str, depth: u32) -> String {
    let (console, visible) = console_window();
    format!(
        "role={role} depth={depth} pid={} session={:?} in_job={:?} console={console} visible={visible}",
        id(),
        current_session(),
        in_job(),
    )
}

fn parent(args: &[String]) -> io::Result<()> {
    let log = &args[1];
    let depth = depth_of(args)?;
    let before = in_job();
    let joined = args
        .iter()
        .any(|arg| arg == "--job")
        .then(|| join_kill_on_close_job().map(|_| "ok"));
    append(
        log,
        &format!(
            "{} in_job_before={before:?} join={joined:?}",
            describe("parent", depth)
        ),
    )?;
    let mut command = child_command(log, depth)?;
    // Like the supervisor: no stdin, and the output is not a console.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if args.iter().any(|arg| arg == "--no-window") {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()?;
    sleep_forever()
}

fn child(args: &[String]) -> io::Result<()> {
    let log = &args[1];
    let depth = depth_of(args)?;
    append(log, &describe("child", depth))?;
    if depth > 1 {
        // A plain spawn, like a program that knows nothing about Marsa.
        child_command(log, depth - 1)?.spawn()?;
    }
    sleep_forever()
}

/// `jobtree.exe child <log> <depth>`: always the console program, next to
/// the running one.
fn child_command(log: &str, depth: u32) -> io::Result<Command> {
    let program = env::current_exe()?.with_file_name("jobtree.exe");
    let mut command = Command::new(program);
    command.args(["child", log, &depth.to_string()]);
    Ok(command)
}

fn sleep_forever() -> ! {
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
