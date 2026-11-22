//! `marsa setup`, `autostart`, and `uninstall --all`.

use std::path::Path;

use marsa_core::types::OpName;

use super::*;

// `marsa setup`.

/// On stderr before the call, which may wait for the admin prompt and then
/// for the trust dialog (macOS).
pub const SETUP_STARTED: &str = "Setting up Marsa. macOS may ask for an administrator \
     password, and may then ask you to approve trusting Marsa's certificate authority for your \
     user.";
/// Windows: the UAC prompt may open behind other windows, as the daemon
/// that asks for it has none.
pub const SETUP_STARTED_WINDOWS: &str = "Setting up Marsa. Windows may ask for admin rights \
     (UAC) once, to install Marsa's helper service; if no prompt shows, look for it in the \
     taskbar.";
/// The same on Linux, with the one thing setup changes for every program
/// (ADR 0014): it says so before the password is asked.
pub const SETUP_STARTED_LINUX: &str = "Setting up Marsa. Linux may ask for your password, \
     and a second time if the .test names need a step of their own. \
     Setup lets every program on this computer use ports 80 to 1023, so that Caddy can serve \
     your sites on ports 80 and 443.";
/// After Marsa stopped waiting for the helper, which may still have run.
pub(super) const SETUP_MAY_HAVE_RUN: &str = "Some steps may already have run with admin rights. Running \
     \"marsa setup\" again is safe: each step checks what is already done.";
/// When setup had no step to report.
pub const SETUP_NOTHING: &str = "Nothing to set up.";
/// Why setup skipped the trust when no Caddy is installed to make the
/// certificate authority. The general text of the code is for
/// `marsa services start caddy`.
pub const SETUP_CADDY_NOT_INSTALLED: &str = "Caddy is not installed, so Marsa's certificate \
     authority does not exist yet. Run \"marsa install caddy\", then run \"marsa setup\" again.";

/// The hosts file, as the user finds it. `cfg!` outside `platform`: a
/// constant text, for the strings of this one module.
pub(super) const HOSTS_FILE: &str = if cfg!(windows) {
    r"C:\Windows\System32\drivers\etc\hosts"
} else {
    "/etc/hosts"
};

/// The most site names [`hosts_adding`] lists.
const HOSTS_LISTED: usize = 5;

/// Before the prompt of `marsa park` and `marsa link`, on stderr: the sites
/// that go into the hosts file.
pub fn hosts_adding(missing: &[String]) -> String {
    let mut names = missing
        .iter()
        .take(HOSTS_LISTED)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if missing.len() > HOSTS_LISTED {
        names.push_str(&format!(", and {} more", missing.len() - HOSTS_LISTED));
    }
    let open = if missing.len() == 1 {
        "it opens"
    } else {
        "they open"
    };
    format!("Adding {names} to {HOSTS_FILE} so that {open}. Linux may ask for your password.")
}

/// When `marsa park` or `marsa link` did not add its sites to the hosts
/// file, on stderr: why, from `code`, and what to do. The site itself is
/// saved, so the texts of setup ("nothing was changed") do not fit here.
pub fn hosts_not_added(code: &str) -> String {
    let reason = match code {
        "setup_cancelled" => "The password prompt was cancelled.".to_owned(),
        "setup_not_authorized" => "Linux did not give Marsa admin rights: there is no password \
             agent (as over SSH), or the password was wrong."
            .to_owned(),
        "setup_running" | "uninstall_running" => {
            "Another setup or uninstall is running.".to_owned()
        }
        other => message(other, &Details::default()),
    };
    format!(
        "marsa: {reason} The new sites do not open until they are in {HOSTS_FILE}: run \"marsa \
         setup\" to add them. [{code}]"
    )
}

/// When `marsa park` or `marsa link` could not ask whether the hosts file
/// has the new sites, on stderr. A warning: the site is saved.
pub fn hosts_not_checked(code: &str) -> String {
    format!(
        "marsa: Marsa could not check whether the new sites are in {HOSTS_FILE}, so they may not \
         open yet. If one does not, run \"marsa setup\". [{code}]"
    )
}

/// After the failed step of `marsa park` or `marsa link`, whose line above
/// says why: what follows, and what to do.
pub fn hosts_not_added_after(code: &str) -> String {
    format!(
        "marsa: The new sites do not open until they are in {HOSTS_FILE}: run \"marsa setup\" to \
         add them. [{code}]"
    )
}

/// A step of `marsa setup`. A step that is not part of setup shows its own
/// name.
pub fn setup_step(op: OpName) -> String {
    match op {
        OpName::TrustInstall => "Trust Marsa's HTTPS certificates".to_owned(),
        OpName::ResolverInstall => "Send .test names to Marsa".to_owned(),
        OpName::PfInstall => "Route ports 80 and 443 to Marsa".to_owned(),
        OpName::ResolvedInstall => "Send .test names to Marsa".to_owned(),
        OpName::PortsInstall => "Let Caddy use ports 80 and 443".to_owned(),
        // Chrome and Firefox on Linux.
        OpName::NssInstall => "Trust Marsa's certificates in Chrome and Firefox".to_owned(),
        // The hosts way on Linux.
        OpName::ResolvedRemove => "Take out Marsa's link for .test names".to_owned(),
        OpName::HostsSync => format!("Put the sites' .test names in {HOSTS_FILE}"),
        OpName::HostsRemove => format!("Take Marsa's names out of {HOSTS_FILE}"),
        other => other.as_str().to_owned(),
    }
}

pub fn setup_done(step: &str) -> String {
    format!("{step}: done.")
}

pub fn setup_unchanged(step: &str) -> String {
    format!("{step}: already done.")
}

pub fn setup_failed(step: &str, reason: &str, code: &str) -> String {
    format!("{step}: failed. {reason} [{code}]")
}

pub fn setup_skipped(step: &str, reason: &str, code: &str) -> String {
    format!("{step}: skipped. {reason} [{code}]")
}

// `marsa autostart`.

/// `marsa autostart`, when the login file starts this Marsa.
pub const AUTOSTART_ON: &str = "Start at login: on. Marsa's tray icon starts when you log in.";
/// `marsa autostart`, when there is no login file.
pub const AUTOSTART_OFF: &str = "Start at login: off. Run \"marsa autostart on\" to turn it on.";
/// `marsa autostart`, when the login file is not the one this Marsa writes.
pub const AUTOSTART_STALE: &str = "Start at login: off. The login file starts Marsa from \
     another folder, or it was changed. Run \"marsa autostart on\" to fix it.";
/// `marsa autostart on`.
pub const AUTOSTART_TURNED_ON: &str =
    "Start at login: on. From your next login, Marsa's tray icon starts by itself.";
/// `marsa autostart off`.
pub const AUTOSTART_TURNED_OFF: &str = "Start at login: off.";

// `marsa uninstall --all`.

/// After Marsa stopped waiting for the helper, which may still have run.
pub(super) const UNINSTALL_MAY_HAVE_RUN: &str = "Some steps may already have run with admin rights. \
     Running \"marsa uninstall --all\" again is safe: each step checks what is already done.";

/// What `marsa uninstall --all` removes, on stderr before the question.
/// `deletable`: the data folder is Marsa's own, not one from `--data-dir`;
/// `delete`: `--delete-data` was given for it.
pub fn uninstall_all_plan(data_dir: &Path, deletable: bool, delete: bool) -> String {
    let data = match (deletable, delete) {
        (true, true) => format!(
            "The data folder {} will be deleted once every step has succeeded, with the \
             databases in it (databases/).",
            data_dir.display()
        ),
        (true, false) => format!(
            "The data folder stays, with the databases in it, unless you choose to delete it: {}",
            data_dir.display()
        ),
        (false, _) => format!(
            "The data folder given with --data-dir stays: {}",
            data_dir.display()
        ),
    };
    // Windows: the CLI runs on the machine marsad runs on, so `cfg!` is the
    // daemon's system too (ADR 0018).
    if cfg!(windows) {
        let lines = [
            "This removes Marsa from this system:".to_owned(),
            "  - the sites' .test names in the hosts file".to_owned(),
            "  - the trust in Marsa's certificate authority, for all users".to_owned(),
            "  - Marsa's helper service, and its folder in Program Files".to_owned(),
            "Then the Marsa daemon stops, with Caddy and PHP. Windows asks for admin rights \
             (UAC) once. Quit the Marsa window and the tray icon first: they would start the \
             daemon again."
                .to_owned(),
            data,
        ];
        let mut text = lines.join("\n");
        text.push('\n');
        return text;
    }
    // Linux: the same, as for Windows above.
    if cfg!(target_os = "linux") {
        let lines = [
            "This removes Marsa from this system:".to_owned(),
            "  - the setting that lets Caddy use ports 80 and 443".to_owned(),
            "  - the setting that sends .test names to Marsa, or the sites' lines in /etc/hosts"
                .to_owned(),
            "  - the trust in Marsa's certificate authority, for the system and in your Chrome \
             and Firefox"
                .to_owned(),
            "  - the start at login".to_owned(),
            "Then the Marsa daemon stops, with Caddy, PHP, PostgreSQL, and the DNS server. Linux \
             may ask for your password once. Quit the Marsa window and the tray icon first: they \
             would start the daemon again."
                .to_owned(),
            data,
        ];
        let mut text = lines.join("\n");
        text.push('\n');
        return text;
    }
    let lines = [
        "This removes Marsa from this system:".to_owned(),
        "  - the redirect of ports 80 and 443, and its job at startup".to_owned(),
        "  - the setting that sends .test names to Marsa (/etc/resolver/test)".to_owned(),
        "  - the trust in Marsa's certificate authority, for all users and for you".to_owned(),
        "  - the start at login".to_owned(),
        "Then the Marsa daemon stops, with Caddy, PHP, PostgreSQL, and the DNS server. macOS \
         asks for an \
         administrator password, and may ask you to approve taking the trust away. Quit the \
         Marsa window and the menu-bar icon first: they would start the daemon again."
            .to_owned(),
        data,
    ];
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The question before anything changes. Only `yes` goes on.
pub const UNINSTALL_ALL_CONFIRM: &str = "Type yes to remove Marsa: ";
/// On stderr before the call, which may wait for the admin prompt and then
/// for the trust dialogs.
pub const UNINSTALL_ALL_STARTED: &str = "Removing Marsa. macOS will ask for an administrator \
     password, and may then ask you to approve taking away the trust in Marsa's certificate \
     authority.";
/// The same on Linux: one password prompt (pkexec).
pub const UNINSTALL_ALL_STARTED_LINUX: &str = "Removing Marsa. Linux may ask for your password \
     once.";
/// The same on Windows: one UAC prompt, which may sit in the taskbar.
pub const UNINSTALL_ALL_STARTED_WINDOWS: &str = "Removing Marsa. Windows will ask for admin \
     rights (UAC) once; if no prompt shows, look for it in the taskbar.";
/// The data folder question, after the daemon stopped. No is the default.
pub const UNINSTALL_ALL_DATA_QUESTION: &str =
    "Delete the data folder (runtimes, settings, the CA, and every database in databases/)? [y/N] ";
/// When a step failed: the step lines above say which.
pub const UNINSTALL_ALL_INCOMPLETE: &str = "Some steps failed, so Marsa is not fully removed. \
     Running \"marsa uninstall --all\" again is safe: each step checks what is already done.";
/// When the daemon's answer never came: the steps may have run or not.
pub const UNINSTALL_ALL_RESULT_UNKNOWN: &str = "Marsa does not know how far the uninstall \
     got. Running \"marsa uninstall --all\" again is safe: each step checks what is already \
     done.";
/// After the daemon stopped, when another started on the same folder.
pub const UNINSTALL_ALL_DAEMON_BACK: &str = "A Marsa daemon started again: the Marsa window \
     or Marsa's icon (in the menu bar or the system tray) starts it. Quit both, then run \"marsa \
     daemon stop\".";
/// The heading of what Marsa does not do itself.
pub const UNINSTALL_ALL_LEFT_TO_DO: &str = "Left to do by hand:";
/// Under [`UNINSTALL_ALL_LEFT_TO_DO`], always: either would start the daemon
/// again.
pub const QUIT_WINDOW_LEFT: &str = "  Quit the Marsa window and Marsa's icon (in the menu \
     bar or the system tray) if they are open.";

/// After Ctrl+C while the daemon stops after its answer.
pub const UNINSTALL_ALL_STOP_CTRL_C: &str = "Stopped waiting. The Marsa daemon may still be \
     stopping: run \"marsa daemon status\" to see. The data folder was kept.";

/// After Ctrl+C at the data folder question: the same as no.
pub fn data_kept_ctrl_c(path: &Path) -> String {
    format!("Kept the data folder {}.", path.display())
}

/// Setup's text for `trust_record_unusable`: the trust folder `dir`
/// could not be used. The uninstall's text is in `known`.
pub fn setup_trust_dir_unusable(dir: &Path) -> String {
    format!(
        "Marsa cannot use its folder {} for the trusted certificate: it must be your own \
         folder, not a link. Fix or delete it, then run \"marsa setup\" again.",
        dir.display()
    )
}

/// After Ctrl+C while the daemon works: it goes on without this command.
pub fn uninstall_all_ctrl_c(log: &Path) -> String {
    format!(
        "Stopped waiting. The uninstall goes on in marsad: an admin prompt that is open can \
         still be cancelled there. Its result is in {}.",
        log.display()
    )
}

/// A step of `marsa uninstall --all`. A step that is not part of the
/// uninstall shows its own name.
pub fn uninstall_all_step(op: OpName) -> String {
    match op {
        OpName::PfRemove => "Remove the redirect of ports 80 and 443".to_owned(),
        OpName::ResolverRemove => "Stop sending .test names to Marsa".to_owned(),
        OpName::TrustRemove => "Remove the trust in Marsa's certificates for all users".to_owned(),
        OpName::UserTrustRemove => "Remove the trust in Marsa's certificates for you".to_owned(),
        // Linux.
        OpName::PortsRemove => "Stop letting Caddy use ports 80 and 443".to_owned(),
        OpName::ResolvedRemove | OpName::HostsRemove => setup_step(op),
        OpName::NssRemove => {
            "Remove the trust in Marsa's certificates from Chrome and Firefox".to_owned()
        }
        OpName::AutostartRemove => "Stop starting Marsa at login".to_owned(),
        OpName::UninstallAll => {
            "Remove Marsa's hosts lines, its certificate authority, and its helper service"
                .to_owned()
        }
        other => other.as_str().to_owned(),
    }
}

pub fn data_deleted(path: &Path) -> String {
    format!("Deleted the data folder {}.", path.display())
}

/// Windows: the data folder this marsa runs from, which Windows cannot
/// delete while it runs.
pub fn data_dir_in_use(path: &Path) -> String {
    format!(
        "Kept the data folder {}: Windows cannot delete it while this marsa runs from it. \
         Delete it yourself once this command has ended.",
        path.display()
    )
}

/// The data folder after a step failed.
pub fn data_kept_for_retry(path: &Path) -> String {
    format!(
        "Kept the data folder {}: running \"marsa uninstall --all\" again needs it.",
        path.display()
    )
}

/// The data folder when every step went well but a daemon still runs on it.
pub fn data_kept_daemon_runs(path: &Path) -> String {
    format!(
        "Kept the data folder {}: a Marsa daemon still runs on it.",
        path.display()
    )
}

/// Under [`UNINSTALL_ALL_LEFT_TO_DO`]: the `PATH` line the install printed
/// for the user to add, for Marsa's default folder `default_dir`.
pub fn path_left(default_dir: Option<&Path>) -> String {
    // Windows: the CLI runs on the machine marsad runs on; its PATH is a
    // setting of the user, not a file.
    let start = if cfg!(windows) {
        "  If you added Marsa to PATH, take it out of your user's environment variables (search \
         for \"environment variables\" in Settings)"
    } else {
        "  If you added Marsa to PATH, delete that line from your shell's startup file, such as \
         ~/.zprofile or ~/.zshrc"
    };
    // Linux, the same way: the usual shell is bash.
    let start = if cfg!(target_os = "linux") {
        "  If you added Marsa to PATH, delete that line from your shell's startup file, such as \
         ~/.bashrc or ~/.profile"
    } else {
        start
    };
    match default_dir {
        Some(dir) => format!(
            "{start}: it names {} and {}.",
            dir.join("bin").display(),
            dir.join("shims").display()
        ),
        None => format!("{start}."),
    }
}

/// Under [`UNINSTALL_ALL_LEFT_TO_DO`], when the data folder stays and is not
/// needed for a retry.
pub fn data_left(path: &Path) -> String {
    format!(
        "  Delete the data folder {} once you no longer need its runtimes, settings, and \
         certificate authority.",
        path.display()
    )
}

/// Under [`UNINSTALL_ALL_LEFT_TO_DO`], when the programs are outside the
/// data folder.
pub fn programs_left(dir: &Path) -> String {
    format!("  Delete Marsa's programs in {}.", dir.display())
}

/// A program on a port the redirect needs, under the skipped step: its
/// netstat name, its PID, and the app it seems to belong to.
pub fn port_held(port: u16, name: Option<&str>, pid: Option<u32>, app: Option<&str>) -> String {
    let name = name.unwrap_or("an unknown program");
    let about = match (pid, app) {
        (Some(pid), Some(app)) => format!(" (PID {pid}, {app})"),
        (Some(pid), None) => format!(" (PID {pid})"),
        (None, Some(app)) => format!(" ({app})"),
        (None, None) => String::new(),
    };
    format!("  Port {port} is used by {name}{about}.")
}
