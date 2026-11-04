//! macOS: the `pf` redirect of ports 80 and 443 to Caddy (ADR 0008).
//!
//! Caddy runs without root on high ports of `127.0.0.1` and `::1`. Four `rdr`
//! rules on `lo0` send ports 80 and 443 there, so `https://shop.test` needs no
//! port. The rules sit in Marsa's own anchor under `com.apple/`, which the
//! default `/etc/pf.conf` already evaluates, so no system file is edited.
//!
//! pf is shared with macOS and other tools, so the helper keeps two rules. It
//! never disables pf (`pfctl -d`). Every `pfctl` call names Marsa's anchor
//! (`-a`), except `-E` and `-X`: without `-a`, `-f` replaces the system's
//! rules and `-F all` flushes them. `pfctl -E` turns pf on and returns a
//! reference token. The helper keeps it in a root-only file and gives it back
//! with `-X <token>`, so pf goes off only when no tool holds a reference.
//!
//! A LaunchDaemon loads the rules again at every boot. It runs a root-owned
//! copy of the helper, never the helper in the app's `bin/` folder: the user
//! can write there, so any user process could swap the binary and get root at
//! the next boot.

use std::io;
use std::path::Path;
use std::time::Duration;

use crate::response::{OpResult, Outcome};
use crate::system::{RunOutput, System};

/// Writing a file of the redirect failed, or the running helper could not be
/// read to copy it.
pub const PF_WRITE: &str = "pf_write";
/// Deleting a file of the redirect failed.
pub const PF_DELETE: &str = "pf_delete";
/// `pfctl -f` did not load the rules file into the anchor.
pub const PF_LOAD_FAILED: &str = "pf_load_failed";
/// `pfctl -E` printed no reference token.
pub const PF_TOKEN_MISSING: &str = "pf_token_missing";
/// After the load, the anchor does not show the four rules.
pub const PF_RULES_MISSING: &str = "pf_rules_missing";
/// `pfctl -F all` on the anchor failed, so nothing else was removed.
pub const PF_FLUSH_FAILED: &str = "pf_flush_failed";
/// `pf-load` found no rules file: `pf_install` has not run.
pub const PF_NOT_INSTALLED: &str = "pf_not_installed";

/// The op name in the result of `marsa-helper pf-load`.
pub const LOAD: &str = "pf_load";
/// The helper's command line argument for the boot job.
pub const COMMAND: &str = "pf-load";
const INSTALL: &str = "pf_install";
const REMOVE: &str = "pf_remove";

/// `/etc/pf.conf` has `rdr-anchor "com.apple/*"`, so pf evaluates this one.
const ANCHOR: &str = "com.apple/250.MarsaRedirect";
const PFCTL: &str = "/sbin/pfctl";
const LAUNCHCTL: &str = "/bin/launchctl";

/// The rules that `pfctl -f` loads into [`ANCHOR`].
const RULES: &str = "/Library/Application Support/Marsa/pf-redirect.rules";
/// The token from `pfctl -E`. Root only: with it, a process could give back
/// Marsa's reference.
const TOKEN: &str = "/Library/Application Support/Marsa/pf-token";
/// The root-owned copy of the helper that the boot job runs.
const COPY: &str = "/Library/PrivilegedHelperTools/dev.marsa.helper";
const PLIST: &str = "/Library/LaunchDaemons/dev.marsa.pf-redirect.plist";
const LABEL: &str = "dev.marsa.pf-redirect";
/// Everything `pf_remove` deletes. The plist goes first, so a boot job is
/// never left that runs a deleted helper.
const FILES: [&str; 4] = [PLIST, COPY, RULES, TOKEN];

/// The largest helper binary that is copied.
const MAX_HELPER_BYTES: u64 = 64 * 1024 * 1024;
/// A token is a `u64`: at most 20 digits and a newline.
const MAX_TOKEN_BYTES: u64 = 32;
const RULE_COUNT: usize = 4;
/// The waits of the boot job before each check of the rules. The order of
/// `com.apple.pfctl` and this job at boot is not known (ADR 0008).
const RECHECKS: [Duration; 2] = [Duration::from_secs(10), Duration::from_secs(30)];

/// Why an op failed. The message names paths and exit codes, never the
/// content of a file or the output of a command.
struct Failure {
    code: &'static str,
    message: String,
}

/// `pf_install`: redirect port 80 to `http_port` and 443 to `https_port`.
pub fn install(http_port: u16, https_port: u16, system: &impl System) -> OpResult {
    finish(INSTALL, install_files(http_port, https_port, system))
}

/// `pf_remove`: flush the anchor, give back the reference, stop the boot job,
/// and delete the files.
pub fn remove(system: &impl System) -> OpResult {
    finish(REMOVE, remove_all(system))
}

/// The boot job, `marsa-helper pf-load`. It loads the rules, then checks them
/// again after each wait in [`RECHECKS`] and loads them again if they are gone,
/// because `com.apple.pfctl` may load `/etc/pf.conf` after it. `sleep` is
/// `std::thread::sleep` in `main` and a no-op in tests.
pub fn boot(system: &impl System, sleep: impl Fn(Duration)) -> OpResult {
    if !system.exists(Path::new(RULES)) {
        let message = format!("{RULES} is missing: pf_install has not run");
        return OpResult::failed(LOAD, PF_NOT_INSTALLED, message);
    }
    let mut loaded = load(system);
    for wait in RECHECKS {
        sleep(wait);
        // A failed load is tried again too: pf may not have been ready.
        if loaded.is_err() || !rules_loaded(system) {
            loaded = load(system);
        }
    }
    finish(LOAD, loaded.map(|()| Outcome::Done))
}

fn install_files(
    http_port: u16,
    https_port: u16,
    system: &impl System,
) -> Result<Outcome, Failure> {
    let exe = system.current_exe().map_err(|err| {
        let message = format!(
            "cannot find the running helper to copy it to {COPY}: {}",
            err.kind()
        );
        failure(PF_WRITE, message)
    })?;
    let helper = system.read(&exe, MAX_HELPER_BYTES).map_err(|err| {
        let message = format!(
            "cannot read {} to copy it to {COPY}: {}",
            exe.display(),
            err.kind()
        );
        failure(PF_WRITE, message)
    })?;
    // The plist comes last, so launchd never finds a job whose program is not
    // there yet.
    let files = [
        (RULES, rules(http_port, https_port).into_bytes(), 0o644),
        (COPY, helper, 0o755),
        (PLIST, plist().into_bytes(), 0o644),
    ];
    // In place only if pf shows the rules and Marsa holds a reference too.
    let in_place = files
        .iter()
        .all(|(path, bytes, _)| same(system, path, bytes))
        && system.exists(Path::new(TOKEN))
        && rules_loaded(system);
    if in_place {
        return Ok(Outcome::Unchanged);
    }
    for (path, bytes, mode) in &files {
        system
            .write_atomic(Path::new(path), bytes, *mode)
            .map_err(|err| write_failed(path, &err))?;
    }
    load(system)?;
    Ok(Outcome::Done)
}

/// Loads [`RULES`] into [`ANCHOR`], takes a new reference, and checks that pf
/// shows the rules. Shared by `pf_install` and the boot job.
fn load(system: &impl System) -> Result<(), Failure> {
    release(system);
    let loaded = system.run(PFCTL, &["-a", ANCHOR, "-f", RULES]);
    if !exited_0(&loaded) {
        let message = format!(
            "pfctl could not load {RULES} into the anchor {ANCHOR} ({})",
            status(&loaded)
        );
        return Err(failure(PF_LOAD_FAILED, message));
    }
    let enabled = system.run(PFCTL, &["-E"]);
    // A printed token is a reference held, whatever the exit code.
    let Some(token) = enabled.as_ref().ok().and_then(token_in) else {
        let message = format!("pfctl -E printed no reference token ({})", status(&enabled));
        return Err(failure(PF_TOKEN_MISSING, message));
    };
    let token = token.to_string();
    let line = format!("{token}\n");
    if let Err(err) = system.write_atomic(Path::new(TOKEN), line.as_bytes(), 0o600) {
        // A token that is not on disk can never be given back, so it goes now.
        // Nothing more can be done if this fails too.
        let _ = system.run(PFCTL, &["-X", &token]);
        return Err(write_failed(TOKEN, &err));
    }
    if !rules_loaded(system) {
        let message = format!("the anchor {ANCHOR} does not show the {RULE_COUNT} redirect rules");
        return Err(failure(PF_RULES_MISSING, message));
    }
    Ok(())
}

fn remove_all(system: &impl System) -> Result<Outcome, Failure> {
    let installed = FILES.iter().any(|path| system.exists(Path::new(path)));
    // `Some(0)` only: if pfctl cannot list the anchor, the flush still runs.
    if !installed && rdr_rules(system) == Some(0) {
        return Ok(Outcome::Unchanged);
    }
    let flushed = system.run(PFCTL, &["-a", ANCHOR, "-F", "all"]);
    if !exited_0(&flushed) {
        // The files stay, so a retry still has the token to give back.
        let message = format!(
            "pfctl could not flush the anchor {ANCHOR} ({})",
            status(&flushed)
        );
        return Err(failure(PF_FLUSH_FAILED, message));
    }
    release(system);
    // Failure is fine: launchd loads the job only at the boot after
    // `pf_install`, so it may not be loaded.
    let _ = system.run(LAUNCHCTL, &["bootout", &format!("system/{LABEL}")]);
    // Every file is tried. The first failure is reported.
    let mut first = None;
    for path in FILES {
        if let Err(err) = system.remove(Path::new(path)) {
            let message = format!("cannot delete {path}: {}", err.kind());
            first.get_or_insert(failure(PF_DELETE, message));
        }
    }
    first.map_or(Ok(Outcome::Done), Err)
}

/// Gives back the reference in [`TOKEN`], if there is one, and deletes the
/// file, so a token is never given back twice: pf may hand the same number to
/// another tool later.
fn release(system: &impl System) {
    if !system.exists(Path::new(TOKEN)) {
        return;
    }
    let token = system
        .read(Path::new(TOKEN), MAX_TOKEN_BYTES)
        .ok()
        .and_then(|bytes| digits(String::from_utf8_lossy(&bytes).trim()));
    if let Some(token) = token {
        // Failure is fine: a token from before a reboot is no longer valid.
        let _ = system.run(PFCTL, &["-X", &token.to_string()]);
    }
    // Best effort: a stale file is replaced by the next token, and
    // `pf_remove` deletes it again and reports a failure.
    let _ = system.remove(Path::new(TOKEN));
}

/// How many `rdr` rules on `lo0` pf shows in [`ANCHOR`], or `None` if pfctl
/// could not list them. pfctl prints rules in its own form (`port = 80`), so
/// only the rule type and the interface are matched.
fn rdr_rules(system: &impl System) -> Option<usize> {
    let listed = system.run(PFCTL, &["-a", ANCHOR, "-s", "nat"]).ok()?;
    if listed.code != Some(0) {
        return None;
    }
    let text = String::from_utf8_lossy(&listed.stdout);
    let is_ours =
        |line: &&str| line.starts_with("rdr ") && line.split_whitespace().any(|w| w == "lo0");
    Some(text.lines().filter(is_ours).count())
}

fn rules_loaded(system: &impl System) -> bool {
    rdr_rules(system) == Some(RULE_COUNT)
}

/// The number in the `Token : <number>` line of `pfctl -E`, on stdout or
/// stderr.
fn token_in(output: &RunOutput) -> Option<u64> {
    [&output.stdout, &output.stderr]
        .into_iter()
        .find_map(|bytes| {
            String::from_utf8_lossy(bytes).lines().find_map(|line| {
                let value = line
                    .trim()
                    .strip_prefix("Token")?
                    .trim_start()
                    .strip_prefix(':')?;
                digits(value.trim())
            })
        })
}

/// Only ASCII digits that fit a `u64`. `parse` alone would accept a `+`.
fn digits(text: &str) -> Option<u64> {
    let all_digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    all_digits.then(|| text.parse().ok()).flatten()
}

/// The four rules of ADR 0008, LF only, with a final newline. They match only
/// `127.0.0.1` and `::1`, so traffic to the LAN address is never redirected.
fn rules(http_port: u16, https_port: u16) -> String {
    format!(
        "rdr pass on lo0 inet  proto tcp from any to 127.0.0.1 port 80  -> 127.0.0.1 port {http_port}\n\
         rdr pass on lo0 inet  proto tcp from any to 127.0.0.1 port 443 -> 127.0.0.1 port {https_port}\n\
         rdr pass on lo0 inet6 proto tcp from any to ::1       port 80  -> ::1       port {http_port}\n\
         rdr pass on lo0 inet6 proto tcp from any to ::1       port 443 -> ::1       port {https_port}\n"
    )
}

/// The boot job. It runs [`COPY`], never the running helper's own path. The
/// values are constants with no `<`, `>`, or `&`, so nothing needs escaping.
fn plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{COPY}</string>
    <string>{COMMAND}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
    )
}

/// The file at `path` has exactly `bytes`. A larger file fails the read.
fn same(system: &impl System, path: &str, bytes: &[u8]) -> bool {
    let max = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    system
        .read(Path::new(path), max)
        .is_ok_and(|current| current == bytes)
}

fn exited_0(result: &io::Result<RunOutput>) -> bool {
    matches!(result, Ok(output) if output.code == Some(0))
}

/// A short reason for a failed `pfctl` call: the exit code, never the output.
fn status(result: &io::Result<RunOutput>) -> String {
    match result {
        Ok(RunOutput {
            code: Some(code), ..
        }) => format!("exit code {code}"),
        Ok(_) => "stopped by a signal".to_owned(),
        Err(err) => format!("cannot run {PFCTL}: {}", err.kind()),
    }
}

fn failure(code: &'static str, message: String) -> Failure {
    Failure { code, message }
}

fn write_failed(path: &str, err: &io::Error) -> Failure {
    failure(PF_WRITE, format!("cannot write {path}: {}", err.kind()))
}

fn finish(op: &'static str, outcome: Result<Outcome, Failure>) -> OpResult {
    match outcome {
        Ok(outcome) => OpResult { op, outcome },
        Err(Failure { code, message }) => OpResult::failed(op, code, message),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use super::*;
    use crate::system::FakeSystem;

    // Literal copies of the constants, so a changed path or flag fails here.
    const PF: &str = "/sbin/pfctl";
    const A: &str = "com.apple/250.MarsaRedirect";
    const RULES_FILE: &str = "/Library/Application Support/Marsa/pf-redirect.rules";
    const LOAD_ARGS: [&str; 4] = ["-a", A, "-f", RULES_FILE];
    const SHOW_ARGS: [&str; 4] = ["-a", A, "-s", "nat"];
    const FLUSH_ARGS: [&str; 4] = ["-a", A, "-F", "all"];

    /// The bytes of the running helper.
    const HELPER: &[u8] = b"\xcf\xfa\xed\xfe the helper binary";
    /// The token in the file of an earlier load, and the one `pfctl -E` prints.
    const OLD: &str = "7393882174112";
    const NEW: &str = "7393882174560";
    /// `pfctl -a <anchor> -s nat` with the four rules. The exact form pfctl
    /// prints is a guess: it needs root to see (ADR 0008).
    const LISTED: &str = "\
rdr pass on lo0 inet proto tcp from any to 127.0.0.1 port = 80 -> 127.0.0.1 port 18080
rdr pass on lo0 inet proto tcp from any to 127.0.0.1 port = 443 -> 127.0.0.1 port 18443
rdr pass on lo0 inet6 proto tcp from any to ::1 port = 80 -> ::1 port 18080
rdr pass on lo0 inet6 proto tcp from any to ::1 port = 443 -> ::1 port 18443
";

    /// A [`FakeSystem`] plus the answers of `pfctl -a <anchor> -s nat`, one per
    /// call, so the rules can vanish between two checks. After the last
    /// answer, the fake's own output is used. Every call is in `fake.log`.
    struct Mac {
        fake: FakeSystem,
        listings: RefCell<VecDeque<&'static str>>,
    }

    impl System for Mac {
        fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
            self.fake.read(path, max)
        }
        fn exists(&self, path: &Path) -> bool {
            self.fake.exists(path)
        }
        fn write_atomic(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
            self.fake.write_atomic(path, bytes, mode)
        }
        fn remove(&self, path: &Path) -> io::Result<()> {
            self.fake.remove(path)
        }
        fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
            let result = self.fake.run(program, args);
            let next = (args == SHOW_ARGS)
                .then(|| self.listings.borrow_mut().pop_front())
                .flatten();
            next.map_or(result, |listing| Ok(ran(0, listing, "")))
        }
        fn current_exe(&self) -> io::Result<PathBuf> {
            self.fake.current_exe()
        }
    }

    fn ran(code: i32, stdout: &str, stderr: &str) -> RunOutput {
        RunOutput {
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// A Mac with nothing installed, where every `pfctl` and `launchctl` call
    /// exits 0, `pfctl -E` prints [`NEW`], and `-s nat` gives `listings`, then
    /// an empty anchor.
    fn fresh(listings: &[&'static str]) -> Mac {
        let mut fake = FakeSystem::default();
        fake.defaults.insert(PF, ran(0, "", ""));
        fake.defaults.insert("/bin/launchctl", ran(0, "", ""));
        let enabled = format!("No ALTQ support in kernel\npf enabled\nToken : {NEW}\n");
        fake.script(PF, &["-E"], ran(0, "", &enabled));
        let exe = fake.current_exe().unwrap();
        fake.files
            .borrow_mut()
            .insert(exe, (HELPER.to_vec(), 0o755));
        let listings = RefCell::new(listings.iter().copied().collect());
        Mac { fake, listings }
    }

    /// [`fresh`], after `pf_install(18080, 18443)`, with [`OLD`] in the token
    /// file.
    fn installed(listings: &[&'static str]) -> Mac {
        let mac = fresh(listings);
        let files = [
            (RULES, rules(18080, 18443).into_bytes(), 0o644),
            (COPY, HELPER.to_vec(), 0o755),
            (PLIST, plist().into_bytes(), 0o644),
            (TOKEN, format!("{OLD}\n").into_bytes(), 0o600),
        ];
        for (path, bytes, mode) in files {
            let file = (bytes, mode);
            mac.fake
                .files
                .borrow_mut()
                .insert(PathBuf::from(path), file);
        }
        mac
    }

    /// Every command run, as `[program, args...]`. Each one must pass
    /// [`is_safe`], so every test checks the pf rules of the module doc.
    fn commands(mac: &Mac) -> Vec<Vec<String>> {
        let log = mac.fake.log.borrow();
        let line = |(program, args): &(String, Vec<String>)| {
            assert!(is_safe(program, args), "{program} {args:?}");
            let mut line = vec![program.clone()];
            line.extend(args.iter().cloned());
            line
        };
        log.iter().map(line).collect()
    }

    /// `pfctl -E`, `pfctl -X <number>`, a `pfctl` call that starts with
    /// `-a <anchor>`, and the `bootout` of the job. No option holds a `d`
    /// (`-d`, also inside `-Ed`). The waits of [`boot`] are logged as `sleep`.
    fn is_safe(program: &str, args: &[String]) -> bool {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let no_disable = |args: &[&str]| {
            let mut options = args.iter().filter_map(|arg| arg.strip_prefix('-'));
            options.all(|flags| !flags.contains('d'))
        };
        match (program, args.as_slice()) {
            ("sleep", [_]) | (PF, ["-E"]) => true,
            ("/bin/launchctl", ["bootout", "system/dev.marsa.pf-redirect"]) => true,
            (PF, ["-X", token]) => digits(token).is_some(),
            (PF, ["-a", A, rest @ ..]) => no_disable(rest),
            _ => false,
        }
    }

    fn pf(args: &[&str]) -> Vec<String> {
        let mut line = vec![PF.to_owned()];
        line.extend(args.iter().map(|arg| (*arg).to_owned()));
        line
    }

    fn bootout() -> Vec<String> {
        let line = ["/bin/launchctl", "bootout", "system/dev.marsa.pf-redirect"];
        line.map(str::to_owned).to_vec()
    }

    fn file(mac: &Mac, path: &str) -> Option<(Vec<u8>, u32)> {
        mac.fake.files.borrow().get(Path::new(path)).cloned()
    }

    fn token_file(token: &str) -> Option<(Vec<u8>, u32)> {
        Some((format!("{token}\n").into_bytes(), 0o600))
    }

    fn result(op: &'static str, outcome: Outcome) -> OpResult {
        OpResult { op, outcome }
    }

    fn code_of(result: &OpResult) -> &'static str {
        match result.outcome {
            Outcome::Failed { code, .. } => code,
            Outcome::Done | Outcome::Unchanged => "ok",
        }
    }

    /// Runs the boot job. Each wait is logged as `sleep <seconds>`.
    fn boot_logged(mac: &Mac) -> OpResult {
        boot(mac, |wait| {
            let line = ("sleep".to_owned(), vec![wait.as_secs().to_string()]);
            mac.fake.log.borrow_mut().push(line);
        })
    }

    /// The snapshot does not show line endings, so they are checked here.
    #[test]
    fn the_rules_file() {
        let text = rules(18080, 18443);
        assert_eq!(text.lines().count(), RULE_COUNT);
        assert!(text.ends_with("18443\n") && !text.contains('\r'));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn the_plist() {
        for value in [LABEL, COPY, COMMAND] {
            assert!(!value.contains(['<', '>', '&']), "{value}");
        }
        insta::assert_snapshot!(plist());
    }

    #[test]
    fn install_writes_the_files_then_loads_the_rules() {
        let mac = fresh(&[LISTED]);
        assert_eq!(install(18080, 18443, &mac), result(INSTALL, Outcome::Done));
        assert_eq!(
            commands(&mac),
            [pf(&LOAD_ARGS), pf(&["-E"]), pf(&SHOW_ARGS)]
        );
        let rules_file = (rules(18080, 18443).into_bytes(), 0o644);
        assert_eq!(file(&mac, RULES_FILE), Some(rules_file));
        let copy = file(&mac, "/Library/PrivilegedHelperTools/dev.marsa.helper");
        assert_eq!(copy, Some((HELPER.to_vec(), 0o755)));
        let plist_file = (plist().into_bytes(), 0o644);
        let launchd = "/Library/LaunchDaemons/dev.marsa.pf-redirect.plist";
        assert_eq!(file(&mac, launchd), Some(plist_file));
        let token = file(&mac, "/Library/Application Support/Marsa/pf-token");
        assert_eq!(token, token_file(NEW));
        // The four files and the running helper.
        assert_eq!(mac.fake.files.borrow().len(), 5);
    }

    /// The app's `bin/` is user-writable: a boot job that ran the helper from
    /// there would give any user process root at the next boot.
    #[test]
    fn the_boot_job_runs_the_root_owned_copy() {
        let mac = fresh(&[LISTED]);
        assert_eq!(install(18080, 18443, &mac).outcome, Outcome::Done);
        let exe = mac.current_exe().unwrap();
        assert_ne!(exe, Path::new(COPY));
        let plist_file = String::from_utf8(file(&mac, PLIST).unwrap().0).unwrap();
        let program = "<array>\n    \
            <string>/Library/PrivilegedHelperTools/dev.marsa.helper</string>\n    \
            <string>pf-load</string>\n  </array>";
        assert!(plist_file.contains(program));
        assert!(!plist_file.contains(exe.to_str().unwrap()));
        assert_eq!(commands(&mac).len(), 3);
    }

    #[test]
    fn install_when_all_is_in_place_is_unchanged() {
        let mut mac = installed(&[LISTED]);
        // Every write fails, so `unchanged` also shows that none was tried.
        for path in [RULES, COPY, PLIST, TOKEN] {
            mac.fake.fail_writes.insert(PathBuf::from(path));
        }
        let expected = result(INSTALL, Outcome::Unchanged);
        assert_eq!(install(18080, 18443, &mac), expected);
        assert_eq!(commands(&mac), [pf(&SHOW_ARGS)]);
        assert_eq!(file(&mac, TOKEN), token_file(OLD));
    }

    #[test]
    fn install_loads_again_when_the_anchor_is_empty() {
        let mac = installed(&["", LISTED]);
        assert_eq!(install(18080, 18443, &mac), result(INSTALL, Outcome::Done));
        let expected = [
            pf(&SHOW_ARGS),
            pf(&["-X", OLD]),
            pf(&LOAD_ARGS),
            pf(&["-E"]),
            pf(&SHOW_ARGS),
        ];
        assert_eq!(commands(&mac), expected);
        assert_eq!(file(&mac, TOKEN), token_file(NEW));
    }

    /// Without a token, Marsa holds no reference, so pf could go off.
    #[test]
    fn install_loads_again_without_a_token() {
        let mac = installed(&[LISTED]);
        mac.fake.files.borrow_mut().remove(Path::new(TOKEN));
        assert_eq!(install(18080, 18443, &mac), result(INSTALL, Outcome::Done));
        assert_eq!(
            commands(&mac),
            [pf(&LOAD_ARGS), pf(&["-E"]), pf(&SHOW_ARGS)]
        );
    }

    #[test]
    fn install_with_another_port_rewrites_the_rules() {
        let mac = installed(&[LISTED]);
        assert_eq!(install(18081, 18443, &mac), result(INSTALL, Outcome::Done));
        let expected = [
            pf(&["-X", OLD]),
            pf(&LOAD_ARGS),
            pf(&["-E"]),
            pf(&SHOW_ARGS),
        ];
        assert_eq!(commands(&mac), expected);
        let rules_file = (rules(18081, 18443).into_bytes(), 0o644);
        assert_eq!(file(&mac, RULES), Some(rules_file));
    }

    #[test]
    fn no_token_from_pfctl_e_is_pf_token_missing_and_writes_no_token_file() {
        let mut mac = fresh(&[LISTED]);
        mac.fake.script(PF, &["-E"], ran(0, "", "pf enabled\n"));
        let message = "pfctl -E printed no reference token (exit code 0)".to_owned();
        let expected = OpResult::failed(INSTALL, PF_TOKEN_MISSING, message);
        assert_eq!(install(18080, 18443, &mac), expected);
        assert_eq!(commands(&mac), [pf(&LOAD_ARGS), pf(&["-E"])]);
        assert_eq!(file(&mac, TOKEN), None);
    }

    #[test]
    fn the_token_is_read_from_stdout_or_stderr() {
        let token = |stdout, stderr| token_in(&ran(0, stdout, stderr));
        assert_eq!(token("Token : 42\n", ""), Some(42));
        assert_eq!(token("", "pf enabled\r\nToken : 42\r\n"), Some(42));
        assert_eq!(token("", "Token:42"), Some(42));
        let bad = [
            "",
            "pf enabled",
            "Token : ",
            "Token : -1",
            "Token : +1",
            "Token : 1 2",
            "Token : 0x1f",
            "Token : 18446744073709551616",
            "MyToken : 1",
        ];
        for bad in bad {
            assert_eq!(token(bad, bad), None, "{bad}");
        }
    }

    /// The message has the exit code, not pfctl's output.
    #[test]
    fn a_failed_load_takes_no_reference() {
        let mut mac = fresh(&[]);
        mac.fake.script(PF, &LOAD_ARGS, ran(1, "", "syntax error"));
        let message =
            format!("pfctl could not load {RULES_FILE} into the anchor {A} (exit code 1)");
        let expected = OpResult::failed(INSTALL, PF_LOAD_FAILED, message);
        assert_eq!(install(18080, 18443, &mac), expected);
        assert_eq!(commands(&mac), [pf(&LOAD_ARGS)]);
        assert_eq!(file(&mac, TOKEN), None);
    }

    #[test]
    fn a_token_that_cannot_be_saved_is_given_back() {
        let mut mac = fresh(&[LISTED]);
        mac.fake.fail_writes.insert(PathBuf::from(TOKEN));
        let message = format!("cannot write {TOKEN}: permission denied");
        let expected = OpResult::failed(INSTALL, PF_WRITE, message);
        assert_eq!(install(18080, 18443, &mac), expected);
        assert_eq!(
            commands(&mac),
            [pf(&LOAD_ARGS), pf(&["-E"]), pf(&["-X", NEW])]
        );
    }

    /// pf holds the reference, so the token is kept to give it back later.
    #[test]
    fn rules_missing_after_the_load_is_pf_rules_missing() {
        let mac = fresh(&[]);
        assert_eq!(code_of(&install(18080, 18443, &mac)), PF_RULES_MISSING);
        assert_eq!(
            commands(&mac),
            [pf(&LOAD_ARGS), pf(&["-E"]), pf(&SHOW_ARGS)]
        );
        assert_eq!(file(&mac, TOKEN), token_file(NEW));
    }

    #[test]
    fn a_failed_write_is_pf_write_and_runs_nothing() {
        for path in [RULES, COPY, PLIST] {
            let mut mac = fresh(&[]);
            mac.fake.fail_writes.insert(PathBuf::from(path));
            let message = format!("cannot write {path}: permission denied");
            let expected = OpResult::failed(INSTALL, PF_WRITE, message);
            assert_eq!(install(18080, 18443, &mac), expected);
            assert!(commands(&mac).is_empty());
        }
        // A helper that cannot be read is not copied, and nothing is written.
        let mac = fresh(&[]);
        mac.fake.files.borrow_mut().clear();
        assert_eq!(code_of(&install(18080, 18443, &mac)), PF_WRITE);
        assert!(commands(&mac).is_empty() && mac.fake.files.borrow().is_empty());
    }

    #[test]
    fn remove_flushes_gives_back_the_token_stops_the_job_and_deletes_the_files() {
        let mac = installed(&[]);
        assert_eq!(remove(&mac), result(REMOVE, Outcome::Done));
        let expected = [pf(&FLUSH_ARGS), pf(&["-X", OLD]), bootout()];
        assert_eq!(commands(&mac), expected);
        // Only the running helper is left.
        let exe = mac.current_exe().unwrap();
        assert!(mac.fake.files.borrow().keys().eq([&exe]));
    }

    #[test]
    fn remove_without_a_token_file_does_not_run_x() {
        let mac = installed(&[]);
        mac.fake.files.borrow_mut().remove(Path::new(TOKEN));
        assert_eq!(remove(&mac), result(REMOVE, Outcome::Done));
        assert_eq!(commands(&mac), [pf(&FLUSH_ARGS), bootout()]);
    }

    #[test]
    fn remove_of_nothing_is_unchanged() {
        let mac = fresh(&[]);
        assert_eq!(remove(&mac), result(REMOVE, Outcome::Unchanged));
        assert_eq!(commands(&mac), [pf(&SHOW_ARGS)]);
    }

    /// Rules with no files, or an anchor pfctl cannot list: flush anyway.
    #[test]
    fn remove_flushes_rules_left_without_files() {
        let mut unknown = fresh(&[]);
        unknown.fake.script(PF, &SHOW_ARGS, ran(1, "", ""));
        for mac in [fresh(&[LISTED]), unknown] {
            assert_eq!(remove(&mac), result(REMOVE, Outcome::Done));
            let expected = [pf(&SHOW_ARGS), pf(&FLUSH_ARGS), bootout()];
            assert_eq!(commands(&mac), expected);
        }
    }

    #[test]
    fn a_failed_flush_keeps_every_file() {
        let mut mac = installed(&[]);
        mac.fake.script(PF, &FLUSH_ARGS, ran(1, "", ""));
        let message = format!("pfctl could not flush the anchor {A} (exit code 1)");
        let expected = OpResult::failed(REMOVE, PF_FLUSH_FAILED, message);
        assert_eq!(remove(&mac), expected);
        assert_eq!(commands(&mac), [pf(&FLUSH_ARGS)]);
        assert_eq!(mac.fake.files.borrow().len(), 5);
    }

    #[test]
    fn a_failed_delete_is_pf_delete_and_the_other_files_still_go() {
        let mut mac = installed(&[]);
        mac.fake.fail_removes.insert(PathBuf::from(PLIST));
        let message = format!("cannot delete {PLIST}: permission denied");
        let expected = OpResult::failed(REMOVE, PF_DELETE, message);
        assert_eq!(remove(&mac), expected);
        assert_eq!(commands(&mac).len(), 3);
        let files = mac.fake.files.borrow();
        assert!(files.contains_key(Path::new(PLIST)) && files.len() == 2);
    }

    /// The rules are there after the load, gone at the 10 s check (as if
    /// `com.apple.pfctl` loaded `/etc/pf.conf`), there after the reload, and
    /// still there at the check after 30 s more.
    #[test]
    fn the_boot_job_loads_again_when_the_rules_vanish() {
        let mac = installed(&[LISTED, "", LISTED, LISTED]);
        assert_eq!(boot_logged(&mac), result(LOAD, Outcome::Done));
        let sleep = |seconds: &str| vec!["sleep".to_owned(), seconds.to_owned()];
        let expected = [
            pf(&["-X", OLD]),
            pf(&LOAD_ARGS),
            pf(&["-E"]),
            pf(&SHOW_ARGS),
            sleep("10"),
            pf(&SHOW_ARGS),
            pf(&["-X", NEW]),
            pf(&LOAD_ARGS),
            pf(&["-E"]),
            pf(&SHOW_ARGS),
            sleep("30"),
            pf(&SHOW_ARGS),
        ];
        assert_eq!(commands(&mac), expected);
        assert_eq!(file(&mac, TOKEN), token_file(NEW));
    }

    /// A failed load is tried again after each wait, and the old token is
    /// given back only once.
    #[test]
    fn the_boot_job_tries_a_failed_load_again() {
        let mut mac = installed(&[]);
        mac.fake.script(PF, &LOAD_ARGS, ran(1, "", ""));
        assert_eq!(code_of(&boot_logged(&mac)), PF_LOAD_FAILED);
        let sleep = |seconds: &str| vec!["sleep".to_owned(), seconds.to_owned()];
        let expected = [
            pf(&["-X", OLD]),
            pf(&LOAD_ARGS),
            sleep("10"),
            pf(&LOAD_ARGS),
            sleep("30"),
            pf(&LOAD_ARGS),
        ];
        assert_eq!(commands(&mac), expected);
        assert_eq!(file(&mac, TOKEN), None);
    }

    #[test]
    fn the_boot_job_without_the_rules_file_does_nothing() {
        let mac = installed(&[]);
        mac.fake.files.borrow_mut().remove(Path::new(RULES));
        let message = format!("{RULES_FILE} is missing: pf_install has not run");
        let expected = OpResult::failed(LOAD, PF_NOT_INSTALLED, message);
        assert_eq!(boot(&mac, |_| panic!("the boot job waited")), expected);
        assert!(commands(&mac).is_empty());
    }

    /// The check that every test runs on its commands catches each case.
    #[test]
    fn the_safety_check_rejects_what_pf_must_never_get() {
        let strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let bad: [&[&str]; 9] = [
            &["-d"],
            &["-E", "-d"],
            &["-Ed"],
            &["-f", RULES_FILE],
            &["-F", "all"],
            &["-a", "com.apple", "-F", "all"],
            &["-a", A, "-d"],
            &["-X", "-d"],
            &["-s", "nat", "-a", A],
        ];
        for args in bad {
            assert!(!is_safe(PF, &strings(args)), "{args:?}");
        }
        assert!(!is_safe("/bin/sh", &strings(&["-c", "pfctl -d"])));
        assert!(is_safe(PF, &strings(&FLUSH_ARGS)));
    }

    /// Real programs that only read: pfctl parses the rules with `-n` and
    /// loads nothing, so it needs no root (ADR 0008).
    #[cfg(target_os = "macos")]
    mod macos {
        use std::fs;

        use super::*;
        use crate::system::RealSystem;

        fn check(program: &'static str, args: &[&str], name: &str, content: &str) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(name);
            fs::write(&path, content).unwrap();
            let mut args = args.to_vec();
            args.push(path.to_str().unwrap());
            let output = RealSystem.run(program, &args).unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.code, Some(0), "{program}: {stderr}");
        }

        #[test]
        fn pfctl_parses_the_rules() {
            let args = ["-n", "-a", ANCHOR, "-f"];
            check(PFCTL, &args, "pf-redirect.rules", &rules(18080, 18443));
        }

        #[test]
        fn plutil_accepts_the_plist() {
            check("/usr/bin/plutil", &["-lint"], "boot.plist", &plist());
        }
    }
}
