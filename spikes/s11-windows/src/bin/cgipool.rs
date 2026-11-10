//! A stand-in for the supervisor: keeps a php-cgi pool running.
//!
//! ```text
//! cgipool <php-cgi.exe> <php.ini> procs|children <n> <base port> <max requests> <log>
//! ```
//!
//! - `procs`: `n` separate php-cgi processes on `n` ports, one request at a
//!   time each (`PHP_FCGI_CHILDREN=0`). Caddy balances between the ports.
//!   This is the plan of architecture.md section 9.
//! - `children`: one php-cgi on one port with `PHP_FCGI_CHILDREN=<n>`. php-cgi
//!   then starts and restarts its own worker processes.
//!
//! php-cgi exits after `PHP_FCGI_MAX_REQUESTS` requests; this program starts
//! it again and writes every start and exit to `<log>`. It joins a
//! kill-on-close job first, so killing it must take every php-cgi with it.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let [program, ini, mode, n, base_port, max_requests, log] = args.as_slice() else {
        return Err(std::io::Error::other("usage: see the top of cgipool.rs"));
    };
    let n: u16 = n.parse().map_err(std::io::Error::other)?;
    let base_port: u16 = base_port.parse().map_err(std::io::Error::other)?;
    let log_file = OpenOptions::new().create(true).append(true).open(log)?;
    let output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{log}.out"))?;
    let log_file = Arc::new(Mutex::new(log_file));
    let say = {
        let log_file = Arc::clone(&log_file);
        move |line: String| {
            let mut file = log_file.lock().unwrap_or_else(|err| err.into_inner());
            let _ = writeln!(file, "{line}");
        }
    };
    say(format!(
        "pool pid={} mode={mode} n={n} job={:?}",
        std::process::id(),
        s11_windows::join_kill_on_close_job().map(|_| "joined"),
    ));

    // (port, PHP_FCGI_CHILDREN) per supervised php-cgi.
    let slots: Vec<(u16, u16)> = match mode.as_str() {
        "procs" => (0..n).map(|i| (base_port + i, 0)).collect(),
        "children" => vec![(base_port, n)],
        _ => return Err(std::io::Error::other("mode is procs or children")),
    };
    let threads: Vec<_> = slots
        .into_iter()
        .map(|(port, children)| {
            let (program, ini, max_requests) = (program.clone(), ini.clone(), max_requests.clone());
            let say = say.clone();
            let output = output.try_clone();
            thread::spawn(move || {
                let Ok(output) = output else { return };
                loop {
                    let (Ok(out), Ok(err)) = (output.try_clone(), output.try_clone()) else {
                        return;
                    };
                    let started = Instant::now();
                    let mut command = Command::new(&program);
                    command.args(["-b", &format!("127.0.0.1:{port}"), "-c", &ini]);
                    command.envs([
                        ("PHP_FCGI_CHILDREN", children.to_string()),
                        ("PHP_FCGI_MAX_REQUESTS", max_requests.clone()),
                    ]);
                    command
                        .stdin(Stdio::null())
                        .stdout(Stdio::from(out))
                        .stderr(Stdio::from(err))
                        .creation_flags(CREATE_NO_WINDOW);
                    let mut child = match command.spawn() {
                        Ok(child) => child,
                        Err(err) => {
                            say(format!("spawn-failed port={port} error={err}"));
                            return;
                        }
                    };
                    say(format!("start port={port} pid={}", child.id()));
                    let status = child.wait();
                    let ran = started.elapsed();
                    say(format!(
                        "exit port={port} pid={} status={status:?} after_ms={}",
                        child.id(),
                        ran.as_millis(),
                    ));
                    // Exit code 0 is php-cgi recycling itself after its
                    // request limit: start it again at once. A php-cgi that
                    // fails (a busy port, a bad ini) must not spin.
                    if !status.is_ok_and(|status| status.success()) {
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        let _ = thread.join();
    }
    Ok(())
}
