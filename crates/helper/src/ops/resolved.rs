//! Linux: names under `.test` go to the DNS server of `marsad`, through a
//! drop-in file of systemd-resolved (ADR 0014). The macOS twin is `resolver`.
//!
//! The file has a port (`DNS=127.0.0.1:18053`), so the DNS server does not
//! need port 53, and `Domains=~test` sends only `.test` names there.
//!
//! systemd-resolved asks one global server at a time. If the machine has
//! another global `DNS=` line, `.test` names never resolve (spike S-02: 0 of
//! 6 tries). So after the restart the helper reads the global servers back,
//! and if Marsa's is not the only one, it takes its file out again and fails
//! with `resolved_other_dns`: a clear error, not a site that does not open.
//!
//! The helper changes the file only if Marsa wrote it ([`marked`]). A failed
//! install leaves no file of Marsa's behind.
//!
//! No message holds the content of a file or the output of a program.

use std::io;
use std::path::Path;

use crate::ops::marked::{self, Current, MARKER};
use crate::ops::program;
use crate::response::{OpResult, Outcome};
use crate::system::{RunOutput, System};

/// A file at [`FILE`] that Marsa did not write, so it was left as it is.
pub const RESOLVED_FOREIGN: &str = "resolved_foreign";
/// [`FILE`] cannot be read, so it was left as it is.
pub const RESOLVED_READ: &str = "resolved_read";
/// systemd-resolved does not run on this machine. Nothing was written.
pub const RESOLVED_NOT_RUNNING: &str = "resolved_not_running";
/// Writing [`FILE`] failed.
pub const RESOLVED_WRITE: &str = "resolved_write";
/// `systemctl restart` failed. After an install, Marsa's file is gone again.
pub const RESOLVED_RESTART_FAILED: &str = "resolved_restart_failed";
/// The machine has another global DNS server, so `.test` names would not
/// resolve. Marsa's file is gone again.
pub const RESOLVED_OTHER_DNS: &str = "resolved_other_dns";
/// The global servers could not be read back, or Marsa's is not among them.
/// Marsa's file is gone again.
pub const RESOLVED_CHECK_FAILED: &str = "resolved_check_failed";
/// Deleting [`FILE`] failed.
pub const RESOLVED_DELETE: &str = "resolved_delete";

/// Marsa's own drop-in. systemd-resolved reads every file in this folder.
const FILE: &str = "/etc/systemd/resolved.conf.d/marsa.conf";
/// Everyone can read it, and only root can change it.
const MODE: u32 = 0o644;
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const RESOLVECTL: &str = "/usr/bin/resolvectl";
const UNIT: &str = "systemd-resolved.service";
/// `restart`, not `reload`: systemd-resolved has no reload before systemd 253.
const RESTART: [&str; 2] = ["restart", UNIT];
/// The line of `resolvectl dns` with the global servers, separated by spaces.
const GLOBAL: &str = "Global:";

const INSTALL: &str = "resolved_install";
const REMOVE: &str = "resolved_remove";

/// `resolved_install`: names under `.test` go to `127.0.0.1:<port>`.
pub fn install(port: u16, system: &impl System) -> OpResult {
    let content = content(port);
    match marked::current(system, Path::new(FILE)) {
        Current::Foreign => {
            // The path only: the content of another tool's file stays out.
            let message = format!("{FILE} was not written by Marsa, so it was left as it is");
            return OpResult::failed(INSTALL, RESOLVED_FOREIGN, message);
        }
        Current::Unreadable(kind) => return unreadable(INSTALL, kind),
        Current::Ours(bytes) if bytes == content.as_bytes() => {
            return result(INSTALL, Outcome::Unchanged);
        }
        Current::Missing | Current::Ours(_) => {}
    }
    if !is_running(system) {
        let message = format!("{UNIT} is not running, so nothing was changed");
        return OpResult::failed(INSTALL, RESOLVED_NOT_RUNNING, message);
    }
    if let Err(err) = system.write_atomic(Path::new(FILE), content.as_bytes(), MODE) {
        let message = format!("cannot write {FILE}: {}", err.kind());
        return OpResult::failed(INSTALL, RESOLVED_WRITE, message);
    }
    let restarted = system.run(SYSTEMCTL, &RESTART);
    let (code, message) = if program::succeeded(&restarted) {
        let listed = system.run(RESOLVECTL, &["dns"]);
        match global_servers(&listed) {
            Some(servers) if servers == [server(port)] => return result(INSTALL, Outcome::Done),
            Some(servers) if servers.contains(&server(port)) => {
                // How many, never which: the addresses are the user's own.
                let others = servers.len() - 1;
                let message = format!(
                    "systemd-resolved has {others} other global DNS server(s), so .test names \
                     would not resolve; {FILE} was removed again"
                );
                (RESOLVED_OTHER_DNS, message)
            }
            Some(_) => {
                let message = format!(
                    "systemd-resolved does not list Marsa's DNS server after the restart; \
                     {FILE} was removed again"
                );
                (RESOLVED_CHECK_FAILED, message)
            }
            None => {
                let reason = if program::succeeded(&listed) {
                    "printed no global line".to_owned()
                } else {
                    program::failure(&listed)
                };
                let message = format!("resolvectl dns {reason}; {FILE} was removed again");
                (RESOLVED_CHECK_FAILED, message)
            }
        }
    } else {
        let message = format!(
            "systemctl restart {UNIT} {}; {FILE} was removed again",
            program::failure(&restarted)
        );
        (RESOLVED_RESTART_FAILED, message)
    };
    // Back to how it was, best effort: the error above is the one to report.
    let _ = system.remove(Path::new(FILE));
    let _ = system.run(SYSTEMCTL, &RESTART);
    OpResult::failed(INSTALL, code, message)
}

/// `resolved_remove`: deletes the file if Marsa wrote it. A missing or
/// foreign file is `unchanged`: nothing of Marsa's is there to remove.
pub fn remove(system: &impl System) -> OpResult {
    match marked::current(system, Path::new(FILE)) {
        Current::Missing | Current::Foreign => return result(REMOVE, Outcome::Unchanged),
        Current::Unreadable(kind) => return unreadable(REMOVE, kind),
        Current::Ours(_) => {}
    }
    if let Err(err) = system.remove(Path::new(FILE)) {
        let message = format!("cannot delete {FILE}: {}", err.kind());
        return OpResult::failed(REMOVE, RESOLVED_DELETE, message);
    }
    // A systemd-resolved that does not run reads its files when it starts.
    if is_running(system) {
        let restarted = system.run(SYSTEMCTL, &RESTART);
        if !program::succeeded(&restarted) {
            let message = format!(
                "{FILE} was deleted, but systemctl restart {UNIT} {}",
                program::failure(&restarted)
            );
            return OpResult::failed(REMOVE, RESOLVED_RESTART_FAILED, message);
        }
    }
    result(REMOVE, Outcome::Done)
}

/// LF only, with a final newline.
fn content(port: u16) -> String {
    format!("{MARKER}\n[Resolve]\nDNS={}\nDomains=~test\n", server(port))
}

/// Marsa's DNS server as systemd-resolved writes it.
fn server(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

fn is_running(system: &impl System) -> bool {
    program::succeeded(&system.run(SYSTEMCTL, &["is-active", "--quiet", UNIT]))
}

/// The global DNS servers in the output of `resolvectl dns`, or `None` if
/// the program failed or printed no [`GLOBAL`] line. Example:
///
/// ```text
/// Global: 1.1.1.1 127.0.0.1:18053
/// Link 2 (eth0): 168.63.129.16
/// ```
fn global_servers(listed: &io::Result<RunOutput>) -> Option<Vec<String>> {
    let output = listed
        .as_ref()
        .ok()
        .filter(|output| output.code == Some(0))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let servers = text.lines().find_map(|line| line.strip_prefix(GLOBAL))?;
    Some(servers.split_whitespace().map(str::to_owned).collect())
}

fn unreadable(op: &'static str, kind: io::ErrorKind) -> OpResult {
    let message = format!("cannot read {FILE}: {kind}, so it was left as it is");
    OpResult::failed(op, RESOLVED_READ, message)
}

fn result(op: &'static str, outcome: Outcome) -> OpResult {
    OpResult { op, outcome }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::ops::apply;
    use crate::os::Os;
    use crate::request::Request;
    use crate::system::FakeSystem;

    // Written out again, not taken from the constants above: a test must
    // fail if one of those changes.
    const CONF: &str = "/etc/systemd/resolved.conf.d/marsa.conf";
    const CTL: &str = "/usr/bin/systemctl";
    const RESOLVE: &str = "/usr/bin/resolvectl";
    const SERVICE: &str = "systemd-resolved.service";
    const IS_ACTIVE: [&str; 3] = ["is-active", "--quiet", SERVICE];
    const RESTART_UNIT: [&str; 2] = ["restart", SERVICE];

    /// The mode of a file a test starts with. Marsa never writes it, so the
    /// mode shows whether an op rewrote the file.
    const OLD_MODE: u32 = 0o600;
    /// Another tool's drop-in at the same path.
    const FOREIGN: &[u8] = b"[Resolve]\nDNS=9.9.9.9\n";

    fn exit(code: i32, stdout: &str) -> RunOutput {
        RunOutput {
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    /// A Linux where systemd-resolved runs, restarts, and lists `global` as
    /// its global DNS servers.
    fn linux(global: &str) -> FakeSystem {
        let mut fake = FakeSystem::default();
        fake.script(CTL, &IS_ACTIVE, exit(0, ""));
        fake.script(CTL, &RESTART_UNIT, exit(0, ""));
        let listed = format!("Global: {global}\nLink 2 (eth0): 168.63.129.16\n");
        fake.script(RESOLVE, &["dns"], exit(0, &listed));
        fake
    }

    fn with_file(fake: FakeSystem, bytes: &[u8]) -> FakeSystem {
        let file = (bytes.to_vec(), OLD_MODE);
        fake.files.borrow_mut().insert(PathBuf::from(CONF), file);
        fake
    }

    fn ours(port: u16) -> Vec<u8> {
        let text =
            "# Managed by Marsa. Do not edit.\n[Resolve]\nDNS=127.0.0.1:PORT\nDomains=~test\n";
        text.replace("PORT", &port.to_string()).into_bytes()
    }

    /// The file at [`CONF`]. No other file may appear.
    fn file(fake: &FakeSystem) -> Option<(Vec<u8>, u32)> {
        let files = fake.files.borrow();
        assert!(files.keys().all(|path| path == Path::new(CONF)));
        files.get(Path::new(CONF)).cloned()
    }

    /// Every command that ran, as `program arguments`.
    fn commands(fake: &FakeSystem) -> Vec<String> {
        let log = fake.log.borrow();
        log.iter()
            .map(|(program, args)| format!("{program} {}", args.join(" ")))
            .collect()
    }

    const ACTIVE: &str = "/usr/bin/systemctl is-active --quiet systemd-resolved.service";
    const RESTARTED: &str = "/usr/bin/systemctl restart systemd-resolved.service";
    const LISTED: &str = "/usr/bin/resolvectl dns";

    fn code_of(result: &OpResult) -> &'static str {
        match result.outcome {
            Outcome::Failed { code, .. } => code,
            Outcome::Done => "done",
            Outcome::Unchanged => "unchanged",
        }
    }

    fn message_of(result: &OpResult) -> &str {
        match &result.outcome {
            Outcome::Failed { message, .. } => message,
            _ => "",
        }
    }

    #[test]
    fn install_writes_the_file_restarts_and_checks_the_servers() {
        let fake = linux("127.0.0.1:18053");
        assert_eq!(install(18053, &fake), result(INSTALL, Outcome::Done));
        assert_eq!(file(&fake), Some((ours(18053), 0o644)));
        assert_eq!(commands(&fake), [ACTIVE, RESTARTED, LISTED]);
    }

    /// The snapshot does not show line endings, so they are checked here.
    #[test]
    fn the_written_file() {
        let fake = linux("127.0.0.1:18053");
        assert_eq!(install(18053, &fake).outcome, Outcome::Done);
        let (bytes, mode) = file(&fake).unwrap();
        assert_eq!(mode, 0o644);
        assert!(bytes.ends_with(b"~test\n") && !bytes.contains(&b'\r'));
        let text = String::from_utf8(bytes).unwrap();
        insta::assert_snapshot!(text);
    }

    #[test]
    fn install_over_our_same_file_is_unchanged_and_runs_nothing() {
        let mut fake = with_file(linux("127.0.0.1:18053"), &ours(18053));
        // Any write fails here, so `unchanged` also shows that none was tried.
        fake.fail_writes.insert(PathBuf::from(CONF));
        assert_eq!(install(18053, &fake), result(INSTALL, Outcome::Unchanged));
        assert_eq!(file(&fake), Some((ours(18053), OLD_MODE)));
        assert!(commands(&fake).is_empty());
    }

    #[test]
    fn install_rewrites_our_file_with_another_port() {
        let fake = with_file(linux("127.0.0.1:18053"), &ours(18054));
        assert_eq!(install(18053, &fake), result(INSTALL, Outcome::Done));
        assert_eq!(file(&fake), Some((ours(18053), MODE)));
        assert_eq!(commands(&fake), [ACTIVE, RESTARTED, LISTED]);
    }

    /// The exact message also shows that it holds the path and no content.
    #[test]
    fn install_fails_on_a_foreign_file_and_keeps_it() {
        let fake = with_file(linux("127.0.0.1:18053"), FOREIGN);
        let message = format!("{CONF} was not written by Marsa, so it was left as it is");
        let expected = OpResult::failed(INSTALL, RESOLVED_FOREIGN, message);
        assert_eq!(install(18053, &fake), expected);
        assert_eq!(file(&fake), Some((FOREIGN.to_vec(), OLD_MODE)));
        assert!(commands(&fake).is_empty());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_resolved_read_and_kept() {
        let mut fake = with_file(linux("127.0.0.1:18053"), &ours(18053));
        fake.fail_reads.insert(PathBuf::from(CONF));
        let message = format!("cannot read {CONF}: permission denied, so it was left as it is");
        let failed = |op| OpResult::failed(op, RESOLVED_READ, message.clone());
        assert_eq!(install(18053, &fake), failed(INSTALL));
        assert_eq!(remove(&fake), failed(REMOVE));
        assert_eq!(file(&fake), Some((ours(18053), OLD_MODE)));
        assert!(commands(&fake).is_empty());
    }

    /// `is-active` exits with 3 for a stopped unit and 4 for one that does
    /// not exist. A machine without systemd has no `systemctl` at all.
    #[test]
    fn without_a_running_resolved_nothing_is_written() {
        let mut stopped = linux("");
        stopped.script(CTL, &IS_ACTIVE, exit(3, ""));
        for fake in [stopped, FakeSystem::default()] {
            let message = format!("{SERVICE} is not running, so nothing was changed");
            let expected = OpResult::failed(INSTALL, RESOLVED_NOT_RUNNING, message);
            assert_eq!(install(18053, &fake), expected);
            assert_eq!(file(&fake), None);
            assert_eq!(commands(&fake), [ACTIVE]);
        }
    }

    #[test]
    fn a_failed_write_is_resolved_write_and_nothing_restarts() {
        let mut fake = linux("127.0.0.1:18053");
        fake.fail_writes.insert(PathBuf::from(CONF));
        let message = format!("cannot write {CONF}: permission denied");
        let expected = OpResult::failed(INSTALL, RESOLVED_WRITE, message);
        assert_eq!(install(18053, &fake), expected);
        assert_eq!(commands(&fake), [ACTIVE]);
    }

    #[test]
    fn a_failed_restart_takes_the_file_out_again() {
        let mut fake = linux("127.0.0.1:18053");
        fake.script(CTL, &RESTART_UNIT, exit(1, ""));
        let failed = install(18053, &fake);
        assert_eq!(code_of(&failed), RESOLVED_RESTART_FAILED);
        assert_eq!(
            message_of(&failed),
            format!("systemctl restart {SERVICE} exited with code 1; {CONF} was removed again")
        );
        assert_eq!(file(&fake), None);
        // The second restart is for the state without the file.
        assert_eq!(commands(&fake), [ACTIVE, RESTARTED, RESTARTED]);
    }

    /// Spike S-02, case T2b: next to another global server, `.test` never
    /// resolves.
    #[test]
    fn another_global_server_is_resolved_other_dns_and_the_file_goes() {
        for (global, others) in [
            ("1.1.1.1 127.0.0.1:18053", 1),
            ("127.0.0.1:18053 1.1.1.1 2606:4700:4700::1111", 2),
        ] {
            let fake = linux(global);
            let failed = install(18053, &fake);
            assert_eq!(code_of(&failed), RESOLVED_OTHER_DNS);
            let message = message_of(&failed);
            assert!(message.contains(&format!("has {others} other global DNS server(s)")));
            // The user's own servers stay out of the message.
            assert!(!message.contains("1.1.1.1") && !message.contains("2606"));
            assert_eq!(file(&fake), None);
            assert_eq!(commands(&fake), [ACTIVE, RESTARTED, LISTED, RESTARTED]);
        }
    }

    #[test]
    fn servers_that_cannot_be_checked_are_resolved_check_failed_and_the_file_goes() {
        let no_program = {
            let mut fake = linux("");
            fake.outputs.retain(|(program, _), _| *program != RESOLVE);
            fake
        };
        let failing = {
            let mut fake = linux("");
            fake.script(RESOLVE, &["dns"], exit(1, "Global: 127.0.0.1:18053\n"));
            fake
        };
        let no_line = {
            let mut fake = linux("");
            fake.script(RESOLVE, &["dns"], exit(0, "Link 2 (eth0): 1.1.1.1\n"));
            fake
        };
        let cases = [
            (no_program, "resolvectl dns could not run: entity not found"),
            (failing, "resolvectl dns exited with code 1"),
            (no_line, "resolvectl dns printed no global line"),
            // Another port, and no server at all.
            (linux("127.0.0.1:18054"), "does not list Marsa's DNS server"),
            (linux(""), "does not list Marsa's DNS server"),
        ];
        for (fake, reason) in cases {
            let failed = install(18053, &fake);
            assert_eq!(code_of(&failed), RESOLVED_CHECK_FAILED, "{reason}");
            assert!(
                message_of(&failed).contains(reason),
                "{}",
                message_of(&failed)
            );
            assert_eq!(file(&fake), None);
            assert_eq!(commands(&fake), [ACTIVE, RESTARTED, LISTED, RESTARTED]);
        }
    }

    #[test]
    fn remove_without_our_file_is_unchanged_and_runs_nothing() {
        for fake in [linux(""), with_file(linux(""), FOREIGN)] {
            let before = file(&fake);
            assert_eq!(remove(&fake), result(REMOVE, Outcome::Unchanged));
            assert_eq!(file(&fake), before);
            assert!(commands(&fake).is_empty());
        }
    }

    #[test]
    fn remove_deletes_our_file_and_restarts() {
        // Also with another port: the marker makes it ours, not the content.
        for port in [18053, 18054] {
            let fake = with_file(linux(""), &ours(port));
            assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
            assert_eq!(file(&fake), None);
            assert_eq!(commands(&fake), [ACTIVE, RESTARTED]);
        }
    }

    #[test]
    fn remove_does_not_restart_a_resolved_that_does_not_run() {
        let mut fake = with_file(linux(""), &ours(18053));
        fake.script(CTL, &IS_ACTIVE, exit(3, ""));
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
        assert_eq!(file(&fake), None);
        assert_eq!(commands(&fake), [ACTIVE]);
    }

    #[test]
    fn a_failed_delete_is_resolved_delete_and_keeps_the_file() {
        let mut fake = with_file(linux(""), &ours(18053));
        fake.fail_removes.insert(PathBuf::from(CONF));
        let message = format!("cannot delete {CONF}: permission denied");
        let expected = OpResult::failed(REMOVE, RESOLVED_DELETE, message);
        assert_eq!(remove(&fake), expected);
        assert_eq!(file(&fake), Some((ours(18053), OLD_MODE)));
        assert!(commands(&fake).is_empty());
    }

    #[test]
    fn a_failed_restart_after_the_delete_is_reported() {
        let mut fake = with_file(linux(""), &ours(18053));
        fake.script(CTL, &RESTART_UNIT, exit(1, ""));
        let failed = remove(&fake);
        assert_eq!(code_of(&failed), RESOLVED_RESTART_FAILED);
        assert_eq!(
            message_of(&failed),
            format!("{CONF} was deleted, but systemctl restart {SERVICE} exited with code 1")
        );
        assert_eq!(file(&fake), None);
    }

    #[test]
    fn the_global_line_of_resolvectl_dns() {
        let servers = |stdout: &str| global_servers(&Ok(exit(0, stdout)));
        let one = Some(vec!["127.0.0.1:18053".to_owned()]);
        assert_eq!(
            servers("Global: 127.0.0.1:18053\nLink 2 (eth0): 1.1.1.1\n"),
            one
        );
        assert_eq!(
            servers("Link 2 (eth0): 1.1.1.1\nGlobal: 127.0.0.1:18053"),
            one
        );
        assert_eq!(servers("Global:\n"), Some(Vec::new()));
        assert_eq!(
            servers("Global: 1.1.1.1 8.8.8.8\n").map(|s| s.len()),
            Some(2)
        );
        // Only at the start of a line, and only with exit code 0.
        assert_eq!(servers(" Global: 127.0.0.1:18053\n"), None);
        assert_eq!(servers("Link 2 (Global:): 1.1.1.1\n"), None);
        assert_eq!(servers(""), None);
        let failed = Ok(exit(1, "Global: 127.0.0.1:18053\n"));
        assert_eq!(global_servers(&failed), None);
        assert_eq!(global_servers(&Err(io::ErrorKind::NotFound.into())), None);
    }

    /// Through `apply`, as a real request: Linux only.
    #[test]
    fn the_ops_run_from_a_request_on_linux_only() {
        let ops = json!([
            { "op": "resolved_install", "port": 18053 },
            { "op": "resolved_remove" },
        ]);
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let fake = linux("127.0.0.1:18053");
        let expected = [
            result(INSTALL, Outcome::Done),
            result(REMOVE, Outcome::Done),
        ];
        assert_eq!(apply(&request, Os::Linux, &fake), expected);
        assert_eq!(file(&fake), None);

        for os in [Os::Macos, Os::Windows] {
            let fake = linux("127.0.0.1:18053");
            let results = apply(&request, os, &fake);
            let codes: Vec<_> = results.iter().map(code_of).collect();
            assert_eq!(codes, [crate::response::code::OP_NOT_SUPPORTED; 2]);
            assert!(commands(&fake).is_empty() && file(&fake).is_none());
        }
    }
}
