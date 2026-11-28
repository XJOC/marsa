//! Windows-only process code (architecture.md section 9, ADR 0017). For
//! `marsad` and for `spawn_marsad` only, so it is not public outside the
//! crate.
//!
//! - [`join_kill_on_close_job`]: the daemon puts itself in a Job Object that
//!   Windows closes when the daemon ends, however it ends. Every process the
//!   daemon starts afterwards is in that job, so Caddy and PHP never outlive
//!   a crashed daemon.
//! - [`spawn_detached`]: starts a program in the background and hands it none
//!   of the caller's handles. std starts every child with
//!   `bInheritHandles = TRUE`, so a daemon started by `marsa park . | more`
//!   kept the pipe open, and the reader waited until the daemon stopped.
//!
//! - [`current_user_sid`], [`run_elevated`], and [`helper_pipe`]: what
//!   `marsad` needs to install and use the helper service (ADR 0018),
//!   public for the `marsa` crate.
//! - [`root_certificates`]: the roots Windows trusts, for PHP's CA bundle.
//! - [`is_elevated`] and [`process_is_elevated`]: whether a process runs
//!   with an administrator's token, which `postgres.exe` refuses.
//! - [`file_info`]: the volume, number, and names of an open file, for
//!   `platform::files::identity`.
//!
//! The only `unsafe` code of `marsa-core` is in this module and its
//! `helper_pipe` (AGENTS.md section 6). The blocks are short, and every
//! Win32 result is checked.

pub mod helper_pipe;

mod detached;
mod elevation;
mod file_info;
mod registry;
mod roots;

pub(crate) use detached::{DetachedProcess, join_kill_on_close_job, spawn_detached};
pub use elevation::{
    Elevated, ElevatedProcess, current_user_sid, is_elevated, process_is_elevated, run_elevated,
};
pub(crate) use file_info::file_info;
pub use registry::vc_runtime_version;
pub use roots::{Roots, root_certificates};

// What the tests below reach through `use super::*`.
#[cfg(test)]
use detached::{MAX_COMMAND_LINE, check_program, command_line};
#[cfg(test)]
use roots::{for_https_servers, store_certificates};
#[cfg(test)]
use std::path::Path;

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

/// `text` as NUL-terminated UTF-16.
fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = text.encode_wide().collect();
    if wide.contains(&0) {
        return Err(invalid("a NUL character in a path or an argument"));
    }
    wide.push(0);
    Ok(wide)
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::io::Read;
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::*;

    /// The tests below start this test program again in a role, the way
    /// `marsa`, `marsad`, and a child of `marsad` run. A role is a "test"
    /// named `role_<name>`, started with `--exact` and one more argument,
    /// `marsa-test-dir=<folder>`. The test harness reads that argument as a
    /// filter that matches no test. Without it (a normal test run), a role
    /// passes at once.
    const DIR_ARG: &str = "marsa-test-dir=";
    /// With this argument, the `job_parent` role joins no job.
    const NO_JOB_ARG: &str = "marsa-test-no-job";

    fn line(program: &str, args: &[&str]) -> String {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let line = command_line(OsStr::new(program), &args).unwrap();
        assert_eq!(line.last(), Some(&0));
        String::from_utf16(&line[..line.len() - 1]).unwrap()
    }

    /// Every Windows has roots for HTTPS, and each is a certificate the
    /// strict parser takes once it is PEM.
    #[test]
    fn the_root_store_gives_certificates() {
        let roots = root_certificates().expect("read the ROOT store");
        assert!(
            roots.trusted.len() >= 5,
            "only {} roots",
            roots.trusted.len()
        );
        // Every Windows keeps some expired legacy roots: the filter acts.
        assert!(roots.left_out >= 1, "no root was left out");
        for der in &roots.trusted {
            let pem = crate::caddy::pem_certificates(std::slice::from_ref(der));
            assert!(crate::helper::certificate_sha256(&pem).is_some(), "{pem}");
        }
    }

    /// A store that does not exist is an error, and opening it creates none.
    #[test]
    fn a_missing_store_is_an_error() {
        use windows_sys::Win32::Security::Cryptography::CERT_SYSTEM_STORE_CURRENT_USER;
        let name = "marsa-test-no-such-store";
        let user = CERT_SYSTEM_STORE_CURRENT_USER;
        assert!(store_certificates(name, user).is_err());
        assert!(
            store_certificates(name, user).is_err(),
            "the first open made it"
        );
    }

    /// The usage decisions, on the test CA in memory (valid to 2126, no
    /// usage of its own): no property, server auth, code signing only, and
    /// an empty list, which Windows reads as "disabled for every use".
    #[test]
    fn only_roots_for_https_servers_are_kept() {
        use windows_sys::Win32::Security::Cryptography::{
            CTL_USAGE, CertCreateCertificateContext, CertFreeCertificateContext,
            CertSetEnhancedKeyUsage, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
        };
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/helper");
        let pem = fs::read_to_string(fixtures.join("test-ca.pem")).unwrap();
        let der = crate::helper::ca::parse_pem(&pem).unwrap();
        let mut server = *b"1.3.6.1.5.5.7.3.1\0";
        let mut signing = *b"1.3.6.1.5.5.7.3.3\0";
        // SAFETY: `der` lives through the call, which copies it into the new
        // context; the context is checked for null and freed at the end.
        let context = unsafe {
            CertCreateCertificateContext(
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                der.as_ptr(),
                der.len() as u32,
            )
        };
        assert!(!context.is_null());
        let with = |oids: &mut [*mut u8]| {
            let usage = CTL_USAGE {
                cUsageIdentifier: oids.len() as u32,
                rgpszUsageIdentifier: oids.as_mut_ptr(),
            };
            // SAFETY: `context` is valid; `usage` and the OID strings it
            // points at live through the call, which copies them into a
            // property of this in-memory context only.
            unsafe {
                assert_ne!(CertSetEnhancedKeyUsage(context, &usage), 0);
                for_https_servers(context)
            }
        };
        // SAFETY: `context` is valid until it is freed below.
        assert!(unsafe { for_https_servers(context) }, "no usage at all");
        assert!(with(&mut [server.as_mut_ptr()]), "server auth");
        assert!(!with(&mut [signing.as_mut_ptr()]), "code signing only");
        assert!(!with(&mut []), "disabled for every use");
        // SAFETY: `context` is valid and freed once, here.
        unsafe {
            CertFreeCertificateContext(context);
        }
    }

    #[test]
    fn the_program_is_always_quoted_and_plain_arguments_are_not() {
        assert_eq!(
            line(r"C:\marsa\marsad.exe", &["--data-dir", r"C:\data"]),
            r#""C:\marsa\marsad.exe" --data-dir C:\data"#
        );
    }

    /// `argv[0]` has its own rule: between quotes, as it is.
    #[test]
    fn the_program_is_written_as_it_is_and_a_quote_in_it_is_refused() {
        assert_eq!(line(r"C:\my data\x.exe", &[]), r#""C:\my data\x.exe""#);
        let err = command_line(OsStr::new(r#"C:\a"b.exe"#), &[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// A batch file would run through `cmd.exe` (CVE-2024-24576), and a
    /// relative path would depend on the working folder.
    #[test]
    fn only_an_absolute_exe_without_quotes_is_started() {
        for good in [r"C:\marsa\marsad.exe", r"C:\marsa\MARSAD.EXE"] {
            assert!(check_program(Path::new(good)).is_ok(), "{good}");
        }
        let bad = [
            "marsad.exe",
            r"marsa\marsad.exe",
            r"C:\marsa\run.bat",
            r"C:\marsa\run.cmd",
            r"C:\marsa\run.CMD",
            r"C:\marsa\marsad",
            r"C:\marsa\marsad.exe.txt",
            r#"C:\ma"rsa\marsad.exe"#,
        ];
        for bad in bad {
            let err = spawn_detached(Path::new(bad), &[], None).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
        }
    }

    #[test]
    fn arguments_are_quoted_like_the_c_runtime_reads_them() {
        let cases = [
            ("", r#""""#),
            ("a b", r#""a b""#),
            ("a\tb", "\"a\tb\""),
            (r"C:\Program Files\x", r#""C:\Program Files\x""#),
            // A backslash before the closing quote is doubled.
            (r"C:\my data\", r#""C:\my data\\""#),
            // Without quotes, backslashes stay as they are.
            (r"C:\data\", r"C:\data\"),
            (r#"say "hi""#, r#""say \"hi\"""#),
            (r#"a\"b"#, r#"a\\\"b"#),
            (r#"a\\"b c"#, r#""a\\\\\"b c""#),
        ];
        for (arg, expected) in cases {
            assert_eq!(line("p", &[arg]), format!(r#""p" {expected}"#), "{arg:?}");
        }
    }

    #[test]
    fn a_nul_or_a_too_long_command_line_is_refused() {
        let nul = OsStr::new("a\0b");
        let long = "a".repeat(MAX_COMMAND_LINE);
        for err in [
            command_line(nul, &[]).unwrap_err(),
            command_line(OsStr::new("p"), &[nul]).unwrap_err(),
            wide(nul).unwrap_err(),
            command_line(OsStr::new("p"), &[OsStr::new(&long)]).unwrap_err(),
        ] {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn a_missing_program_is_not_found() {
        let temp = TempDir::new().unwrap();
        let missing = temp.path().join("no-such-program.exe");
        let err = spawn_detached(&missing, &[], None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    /// The arguments that make this test program run only the role `role`
    /// in `dir`.
    fn role_args(role: &str, dir: &Path) -> Vec<String> {
        vec![
            "--exact".to_owned(),
            format!("platform::windows::tests::role_{role}"),
            "--nocapture".to_owned(),
            format!("{DIR_ARG}{}", dir.display()),
        ]
    }

    /// The folder of the running role, or `None` in a normal test run.
    fn role_dir() -> Option<PathBuf> {
        env::args().find_map(|arg| arg.strip_prefix(DIR_ARG).map(PathBuf::from))
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stays until the file `stop` appears in `dir`, at most a minute, so a
    /// failed test leaves no process behind for long.
    fn stay_until_stopped(dir: &Path) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !dir.join("stop").exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Role `job_parent`: joins the job, starts `job_child` the way the
    /// supervisor starts Caddy, and stays, like the daemon.
    #[test]
    fn role_job_parent() {
        let Some(dir) = role_dir() else {
            return;
        };
        if !env::args().any(|arg| arg == NO_JOB_ARG) {
            join_kill_on_close_job().unwrap();
            // A second call gives the same answer (`OnceLock`: no second job).
            join_kill_on_close_job().unwrap();
        }
        let mut child = Command::new(env::current_exe().unwrap())
            .args(role_args("job_child", &dir))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        stay_until_stopped(&dir);
        // Reached only when the test did not kill this role.
        let _ = child.wait();
    }

    /// Role `job_child`: listens on a loopback port, writes the port to the
    /// file `port`, and stays. While it lives, the port accepts.
    #[test]
    fn role_job_child() {
        let Some(dir) = role_dir() else {
            return;
        };
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        fs::write(dir.join("port.tmp"), port.to_string()).unwrap();
        fs::rename(dir.join("port.tmp"), dir.join("port")).unwrap();
        stay_until_stopped(&dir);
    }

    /// Whether something accepts on `port`. With a short limit: on Windows a
    /// refused loopback connect is retried for about 2 s, and a live
    /// listener answers at once.
    fn accepts(port: u16) -> bool {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok()
    }

    /// Kills the parent and says whether the child's port still accepts two
    /// seconds later.
    fn child_outlives_a_killed_parent(with_job: bool) -> bool {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let mut parent = Command::new(env::current_exe().unwrap());
        parent.args(role_args("job_parent", dir));
        if !with_job {
            parent.arg(NO_JOB_ARG);
        }
        let mut parent = parent
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let port_file = dir.join("port");
        wait_for("the child's port", || port_file.is_file());
        let port: u16 = fs::read_to_string(&port_file).unwrap().parse().unwrap();
        let accepts = || accepts(port);
        assert!(accepts(), "the child listens before the parent is killed");

        // `TerminateProcess`, like "End task": the parent runs no clean-up.
        parent.kill().unwrap();
        parent.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while accepts() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let outlived = accepts();

        // A child that is still there ends now, before the folder goes.
        fs::write(dir.join("stop"), "").unwrap();
        wait_for("the child's exit", || !accepts());
        outlived
    }

    #[test]
    fn a_killed_process_takes_its_children_with_it_through_the_job() {
        assert!(!child_outlives_a_killed_parent(true));
    }

    /// The control: without the job, the same child stays.
    #[test]
    fn without_the_job_a_child_outlives_its_killed_parent() {
        assert!(child_outlives_a_killed_parent(false));
    }

    /// Role `piped_parent`: prints a line, starts `detached_child` with
    /// [`spawn_detached`], and exits at once, like `marsa` starting `marsad`.
    #[test]
    fn role_piped_parent() {
        let Some(dir) = role_dir() else {
            return;
        };
        println!("parent started");
        let args = role_args("detached_child", &dir);
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        spawn_detached(&env::current_exe().unwrap(), &args, Some(&dir)).unwrap();
    }

    /// Role `detached_child`: prints without standard handles, writes its
    /// working folder to `child-cwd`, stays until `stop` appears, and then
    /// writes `child-done`.
    #[test]
    fn role_detached_child() {
        let Some(dir) = role_dir() else {
            return;
        };
        println!("this goes nowhere");
        eprintln!("this goes nowhere");
        let cwd = env::current_dir().unwrap();
        fs::write(dir.join("child-cwd.tmp"), cwd.to_string_lossy().as_bytes()).unwrap();
        fs::rename(dir.join("child-cwd.tmp"), dir.join("child-cwd")).unwrap();
        stay_until_stopped(&dir);
        fs::write(dir.join("child-done"), "done").unwrap();
    }

    /// The M3 follow-up: a reader of the parent's stdout sees its end when
    /// the parent exits, while the detached child still runs.
    #[test]
    fn a_detached_process_does_not_hold_the_callers_pipe() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let mut parent = Command::new(env::current_exe().unwrap())
            .args(role_args("piped_parent", dir))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = parent.stdout.take().unwrap();
        let started = Instant::now();
        let mut output = String::new();
        // Returns when every copy of the pipe's write end is closed.
        stdout.read_to_string(&mut output).unwrap();
        let waited = started.elapsed();
        assert!(parent.wait().unwrap().success());
        assert!(output.contains("parent started"), "{output}");

        // The child runs on, in the folder it was given, and printing
        // without standard handles did not stop it.
        wait_for("the child's folder", || dir.join("child-cwd").is_file());
        assert!(!dir.join("child-done").exists());
        assert!(
            waited < Duration::from_secs(15),
            "the reader waited {waited:?}: the child held the pipe"
        );
        // Compared only, never handed to a program (`\\?\` paths).
        let cwd = fs::read_to_string(dir.join("child-cwd")).unwrap();
        assert_eq!(
            fs::canonicalize(cwd).unwrap(),
            fs::canonicalize(dir).unwrap()
        );

        fs::write(dir.join("stop"), "").unwrap();
        wait_for("the child's exit", || dir.join("child-done").is_file());
        // The folder is the child's working folder until it has exited, and
        // Windows keeps a folder in use. Gone here, so the drop leaves none.
        wait_for("the folder to be free", || fs::remove_dir_all(dir).is_ok());
    }

    /// Role `exiting_child`: says it runs, waits for `stop`, and exits with
    /// the code of a second `marsad` that found the lock taken, which the
    /// client reads through `try_wait`.
    #[test]
    fn role_exiting_child() {
        let Some(dir) = role_dir() else {
            return;
        };
        fs::write(dir.join("started"), "").unwrap();
        stay_until_stopped(&dir);
        std::process::exit(super::super::spawn::EXIT_ALREADY_RUNNING);
    }

    #[test]
    fn try_wait_reports_the_exit_code_only_after_the_exit() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let args = role_args("exiting_child", dir);
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let mut child = spawn_detached(&env::current_exe().unwrap(), &args, None).unwrap();
        assert_ne!(child.id(), 0);
        wait_for("the child's start", || dir.join("started").is_file());
        assert!(child.try_wait().unwrap().is_none());

        fs::write(dir.join("stop"), "").unwrap();
        let mut status = None;
        wait_for("the child's exit", || {
            status = child.try_wait().unwrap();
            status.is_some()
        });
        assert_eq!(status.unwrap().code(), Some(3));
    }

    /// Arguments that the quoting must carry. None starts with `-`: the
    /// test harness would read it as an option.
    const AWKWARD: [&str; 10] = [
        "",
        "a b",
        "a\tb",
        r#"say "hi""#,
        r#"a\"b"#,
        r"C:\my data\",
        r"C:\data\",
        r#"a\\"b c"#,
        "\u{645}\u{631}\u{633}\u{649} \u{fc}",
        "end",
    ];

    /// Role `echo_args`: writes the arguments after its own four to
    /// `args.json`, then exits.
    #[test]
    fn role_echo_args() {
        let Some(dir) = role_dir() else {
            return;
        };
        let args: Vec<String> = env::args().skip(5).collect();
        let json = serde_json::to_string(&args).unwrap();
        fs::write(dir.join("args.json.tmp"), json).unwrap();
        fs::rename(dir.join("args.json.tmp"), dir.join("args.json")).unwrap();
    }

    /// The round trip: what `spawn_detached` sends is what the program reads.
    #[test]
    fn arguments_reach_the_program_as_they_were_sent() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let mut args = role_args("echo_args", dir);
        args.extend(AWKWARD.map(str::to_owned));
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let mut child = spawn_detached(&env::current_exe().unwrap(), &args, None).unwrap();
        wait_for("the child's exit", || child.try_wait().unwrap().is_some());
        let json = fs::read_to_string(dir.join("args.json")).unwrap();
        let read: Vec<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(read, AWKWARD);
    }

    /// Refused before any prompt: only an absolute `.exe` runs elevated.
    #[test]
    fn only_an_absolute_exe_is_run_elevated() {
        let limit = Duration::from_secs(1);
        let windows = env::var_os("SystemRoot").map(PathBuf::from).unwrap();
        for program in [
            PathBuf::from("marsa-helper.exe"),
            windows.join("System32").join("cmd.bat"),
            windows.join("notepad"),
        ] {
            let err = run_elevated(&program, &[], limit).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidInput,
                "{}",
                program.display()
            );
        }
    }

    /// Read only: this process's user, in the form the helper service
    /// compares (ADR 0018).
    #[test]
    fn the_current_user_sid_is_a_user_sid() {
        let sid = current_user_sid().unwrap();
        // A local or domain account, or a Microsoft Entra ID one: the
        // shapes the helper's `install` accepts.
        let user = sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-12-1-");
        assert!(user, "{sid}");
        assert!(
            sid.bytes()
                .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
        );
    }

    /// Read only. The runner's account is an administrator with UAC off, so
    /// there every process is elevated, as a test with `postgres.exe` found.
    #[test]
    fn a_process_reads_the_same_elevation_by_its_token_and_by_its_pid() {
        let own = is_elevated().unwrap();
        assert_eq!(process_is_elevated(std::process::id()).unwrap(), own);
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            assert!(own, "the runner's processes run elevated");
        }
        // Pid 0 is the idle process, which no program may open.
        assert!(process_is_elevated(0).is_err());
    }

    /// Read only. GitHub's Windows images install the runtime, so there it
    /// must be found and new enough: that proves the key, the views, and the
    /// value types. Another machine may have none.
    #[test]
    fn the_vc_runtime_version_is_read_from_the_registry() {
        let found = vc_runtime_version();
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            let (major, minor, _) = found.expect("GitHub's image has the runtime");
            assert!((major, minor) >= crate::doctor::VC_RUNTIME_MIN, "{found:?}");
        } else if let Some((major, ..)) = found {
            assert_eq!(major, 14);
        }
    }
}
