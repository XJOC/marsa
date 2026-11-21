//! Running the helper (`marsa-helper apply <request>`) with administrator
//! privileges, and reading its answer. The helper file is checked before any
//! prompt: it is there, it can be changed only by its owner (this user or
//! root), and on macOS it is not inside `~/Desktop`, `~/Documents`, or
//! `~/Downloads` (setup only: macOS keeps root out of those folders, so the
//! boot job's copy of the helper fails there). The helper's JSON answer gives
//! one result per operation, in the order sent. No operation left: no prompt.

use super::Flow;
use super::Reason;
use super::Setup;
use super::failure;
use super::osascript::OSASCRIPT;
use super::osascript::osascript_args;
use super::osascript::read_answer_for;
use super::pkexec;
use super::short_code;
use super::steps::UNINSTALL_ALL;
use super::steps::inside;
use super::windows;
use marsa_core::codes;
use marsa_core::helper::HELPER_PROTOCOL_VERSION;
use marsa_core::helper::HelperOp;
use marsa_core::helper::HelperRequest;
use marsa_core::helper::HelperResponse;
use marsa_core::helper::OpResult;
use marsa_core::recipes::Os;
use marsa_core::recipes::Platform;
use marsa_core::types::ApiError;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How long `osascript` may take: the user types the password meanwhile.
/// The trust dialog of `security add-trusted-cert` gets the same.
pub(in crate::daemon) const PROMPT_LIMIT: Duration = Duration::from_secs(5 * 60);

/// The largest helper answer read. A real one is well under 4 KiB.
pub(super) const MAX_ANSWER: usize = 64 * 1024;

/// The folders under the home folder that macOS keeps other users, root
/// included, out of until the user allows it (the folder protection of
/// Privacy & Security). Root could not read a helper there to copy it for
/// the boot job: `pf_write` in task M4-17, `pf_helper_unreadable` since
/// task M4-27.
const PROTECTED_FOLDERS: [&str; 3] = ["Desktop", "Documents", "Downloads"];

/// Where an install puts the programs, for the message that says to move
/// Marsa there.
pub(super) const INSTALL_BIN: &str = "~/Library/Application Support/dev.marsa.Marsa/bin";

/// The folder of [`PROTECTED_FOLDERS`] under `home` that holds `helper`, if
/// any, as the helper's path writes it. By component, so `~/Desktop2` is not
/// `~/Desktop`. The folder's name is compared without ASCII case: the usual
/// macOS disk takes `~/desktop` for `~/Desktop`. No `canonicalize` and no
/// I/O: the path is compared as root would open it.
fn protected_folder(helper: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    let first = helper.strip_prefix(home).ok()?.components().next()?;
    let name = first.as_os_str().to_str()?;
    PROTECTED_FOLDERS
        .iter()
        .any(|protected| name.eq_ignore_ascii_case(protected))
        .then(|| home.join(name))
}

/// Why `marsa-helper` must not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HelperFault {
    /// No file at the path, or the path is not absolute.
    Missing,
    /// Another user could change the file or put another in its place.
    Unsafe(&'static str),
}

/// Checks the helper file before it runs as root. It must be a regular file
/// (not a symlink), and on Unix the file and its folder must belong to this
/// user or root and be writable by nobody else. Another user could otherwise
/// replace it before the user approves the prompt.
///
/// What this cannot stop: a program of this same user replacing the file
/// between this check and the prompt. Only a copy owned by root would.
fn check_helper(helper: &Path) -> Result<(), HelperFault> {
    if !helper.is_absolute() {
        return Err(HelperFault::Missing);
    }
    // `symlink_metadata`: a symlink is refused, not followed.
    let meta = match fs::symlink_metadata(helper) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(HelperFault::Missing),
        Err(_) => return Err(HelperFault::Unsafe("the file cannot be checked")),
    };
    if !meta.file_type().is_file() {
        return Err(HelperFault::Unsafe("the file is not a regular file"));
    }
    check_owners(helper, &meta)
}

/// The file and its folder: owned by this user or root, and writable by
/// nobody else. The mode check is the one of `platform::daemon_file`.
// `#[cfg]` outside `platform`: Unix owners and modes, read with
// `MetadataExt`. The Windows twin is below.
#[cfg(unix)]
pub(super) fn check_owners(helper: &Path, file: &fs::Metadata) -> Result<(), HelperFault> {
    use std::os::unix::fs::MetadataExt;

    let folder = helper
        .parent()
        .and_then(|folder| fs::metadata(folder).ok())
        .ok_or(HelperFault::Unsafe("the folder cannot be checked"))?;
    let me = current_uid().map_err(|_| HelperFault::Unsafe("the current user is not known"))?;
    let checks = [
        (
            file,
            "the file is writable by group or others",
            "the file belongs to another user",
        ),
        (
            &folder,
            "the folder is writable by group or others",
            "the folder belongs to another user",
        ),
    ];
    for (meta, writable, foreign) in checks {
        // `0o022`: write for the group or for others.
        if meta.mode() & 0o022 != 0 {
            return Err(HelperFault::Unsafe(writable));
        }
        if meta.uid() != me && meta.uid() != 0 {
            return Err(HelperFault::Unsafe(foreign));
        }
    }
    Ok(())
}

/// `#[cfg]` outside `platform`: this check reads Unix owners and modes.
/// Windows has none of them; `launch` checks there that the helper is in the
/// user's own folder instead (ADR 0018).
#[cfg(not(unix))]
pub(super) fn check_owners(_helper: &Path, _file: &fs::Metadata) -> Result<(), HelperFault> {
    Ok(())
}

/// The effective user of this process. Std has no `geteuid` and `libc` is
/// not an approved dependency (AGENTS.md section 8), so this reads the owner
/// of a new pipe: macOS and Linux give a pipe the effective user of the
/// process that asks. A test compares it with a new file's owner.
// `#[cfg]` outside `platform`: only `check_owners`, the trust folder check
// of `user_trust`, and the owner checks of `nss` need it, on Unix.
#[cfg(unix)]
pub(super) fn current_uid() -> io::Result<u32> {
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::MetadataExt;

    let (reader, _writer) = io::pipe()?;
    Ok(File::from(OwnedFd::from(reader)).metadata()?.uid())
}

/// The name of `op` in the helper's results: its `op` tag in JSON.
pub(super) fn op_name(op: &HelperOp) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(op)?;
    match value.get("op").and_then(serde_json::Value::as_str) {
        Some(name) => Ok(name.to_owned()),
        None => Err(serde::de::Error::custom("an operation without its name")),
    }
}

/// Whether the helper answered for exactly the operations sent, in order.
/// `uninstall_all` gives one result per removal it runs, so a request with
/// it is not checked; setup never sends it, and the uninstall sends it alone.
pub(super) fn results_match(names: &[String], results: &[OpResult]) -> bool {
    names.iter().any(|name| name == UNINSTALL_ALL)
        || names
            .iter()
            .map(String::as_str)
            .eq(results.iter().map(|result| result.op.as_str()))
}

/// The results in the helper's `answer`, or why there are none, with the
/// error codes of `flow`. `exit` is the exit code it came with, for the log.
pub(super) fn results_of(
    flow: Flow,
    answer: HelperResponse,
    exit: Option<i32>,
) -> Result<Vec<OpResult>, ApiError> {
    let noun = flow.noun();
    if let Some(refused) = answer.error {
        let code = short_code(&refused.code);
        tracing::warn!(code = %code, "marsa-helper refused the request of {noun}");
        let message = format!("the helper refused the request: {code}");
        return Err(failure(flow.code(Reason::HelperRejected), message));
    }
    if answer.results.is_empty() {
        tracing::warn!(?exit, "marsa-helper answered with no result");
        return Err(failure(
            flow.code(Reason::HelperFailed),
            "the helper gave no result",
        ));
    }
    Ok(answer.results)
}

/// The helper's answer in `json`, if it is one of this protocol version.
pub(super) fn parse_answer(json: &[u8]) -> Option<HelperResponse> {
    let answer: HelperResponse = serde_json::from_slice(json).ok()?;
    (answer.version == HELPER_PROTOCOL_VERSION).then_some(answer)
}

/// From the first `{` to the last `}`: the helper's JSON inside the error
/// line of `osascript`, whose other parts hold no brace.
pub(super) fn braces(text: &[u8]) -> Option<&[u8]> {
    let start = text.iter().position(|&byte| byte == b'{')?;
    let end = text.iter().rposition(|&byte| byte == b'}')?;
    text.get(start..=end)
}

impl Setup {
    /// The helper, checked before any prompt: there, not in a folder macOS
    /// keeps root out of (setup only), in the user's own folder on Windows,
    /// and safe from other users. Each refusal has its code of `flow`.
    pub(super) fn checked_helper(&self, flow: Flow, platform: Platform) -> Result<&Path, ApiError> {
        let missing = || {
            tracing::warn!(path = ?self.helper, "marsa-helper is not next to marsad");
            failure(
                flow.code(Reason::HelperMissing),
                "marsa-helper is not next to marsad",
            )
        };
        let helper = self.helper.as_deref().ok_or_else(missing)?;
        // First: it reads nothing, and the advice to move Marsa comes before
        // any word about the file itself.
        let protected = (flow == Flow::Setup && platform.os() == Os::Mac)
            .then(|| protected_folder(helper, self.home.as_deref()))
            .flatten();
        if let Some(folder) = protected {
            tracing::warn!(
                path = %helper.display(),
                folder = %folder.display(),
                "marsa-helper is in a folder macOS keeps root out of; no admin prompt"
            );
            let message = format!(
                "marsa-helper is inside {}, a folder macOS protects, so root could not read \
                 it to set up the boot job; run \"marsa daemon stop\", move Marsa to \
                 {INSTALL_BIN}, and run \"marsa setup\" from there",
                folder.display()
            );
            return Err(failure(codes::SETUP_HELPER_PROTECTED_FOLDER, message));
        }
        // Windows: no owner check like the one of Unix below. The user's own
        // folder is writable by this user only, and the user runs what they
        // choose anyway (ADR 0018); a folder elsewhere (under `C:\`, say)
        // may let any user of the PC change the program behind the prompt.
        if platform.os() == Os::Windows
            && !self
                .home
                .as_deref()
                .is_some_and(|home| inside(helper, home))
        {
            tracing::warn!(
                path = %helper.display(),
                "marsa-helper is outside this user's folder; no UAC prompt"
            );
            let message = "marsa-helper is outside your user folder, where another user could \
                           change it";
            return Err(failure(flow.code(Reason::HelperUnsafe), message));
        }
        match check_helper(helper) {
            Ok(()) => {}
            Err(HelperFault::Missing) => return Err(missing()),
            Err(HelperFault::Unsafe(reason)) => {
                tracing::warn!(
                    path = %helper.display(),
                    reason,
                    "marsa-helper could be changed by another user; no admin prompt"
                );
                let message = format!("marsa-helper is not safe to run as root: {reason}");
                return Err(failure(flow.code(Reason::HelperUnsafe), message));
            }
        }
        Ok(helper)
    }

    /// Sends `ops` to the helper behind the admin prompt and reads its
    /// answer, with the error codes of `flow`. With no operation nothing
    /// runs and no prompt shows. The helper is checked first: a file that is
    /// missing or that another user could change means no prompt, and so
    /// does, for setup only, a path in a folder macOS keeps root out of (on
    /// macOS; by the path alone, before any look at the file).
    ///
    /// The uninstall skips the folder check: root runs a helper from those
    /// folders through `osascript` (task M4-17: `resolver_install` worked
    /// from `~/Desktop`), and only the boot job's copy of the binary, which
    /// `pf_install` reads, fails there. `uninstall_all` reads no binary, so
    /// moving Marsa first would only be in the user's way.
    pub(in crate::daemon) fn launch(
        &self,
        flow: Flow,
        ops: Vec<HelperOp>,
        platform: Platform,
        stop: &CancellationToken,
    ) -> Result<Vec<OpResult>, ApiError> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let helper = self.checked_helper(flow, platform)?;
        let cannot_write = |err: serde_json::Error| {
            tracing::error!(error = %err, "cannot write the helper request");
            failure(
                flow.code(Reason::HelperFailed),
                "cannot write the helper request",
            )
        };
        let names = ops
            .iter()
            .map(op_name)
            .collect::<Result<Vec<_>, _>>()
            .map_err(cannot_write)?;
        // Windows: the service trusts only the CA named at its install, so it
        // is installed for the CA this request trusts, if any.
        let ca_sha256 = ops.iter().find_map(|op| match op {
            HelperOp::TrustInstall { pem } if platform.os() == Os::Windows => {
                marsa_core::helper::certificate_sha256(pem)
            }
            _ => None,
        });
        let request = serde_json::to_string(&HelperRequest::new(ops)).map_err(cannot_write)?;
        let results = if platform.os() == Os::Windows {
            let Some(service) = self.windows.as_deref() else {
                tracing::error!("no helper service on this system");
                return Err(failure(
                    flow.code(Reason::HelperFailed),
                    "no helper service here",
                ));
            };
            windows::launch(flow, service, helper, &request, ca_sha256.as_deref(), stop)?
        } else if platform.os() == Os::Linux {
            tracing::info!(helper = %helper.display(), "asking for admin rights to run marsa-helper");
            let args = pkexec::args(helper, &request);
            let answer = self.runner.run(pkexec::PKEXEC, &args, PROMPT_LIMIT, stop);
            pkexec::read_answer(flow, answer)?
        } else {
            tracing::info!(helper = %helper.display(), "asking for admin rights to run marsa-helper");
            let args = osascript_args(helper, &request);
            let answer = self.runner.run(OSASCRIPT, &args, PROMPT_LIMIT, stop);
            read_answer_for(flow, answer)?
        };
        if !results_match(&names, &results) {
            tracing::warn!(
                sent = ?names,
                got = ?results.iter().map(|result| short_code(&result.op)).collect::<Vec<_>>(),
                "marsa-helper answered for other operations than it was sent"
            );
            let message = "the helper's results do not match the request";
            return Err(failure(flow.code(Reason::HelperFailed), message));
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::super::ports::NETSTAT;
    use super::super::test_support::*;

    use super::*;

    use marsa_core::caddy::CaddyInput;

    use marsa_core::recipes::Platform;

    #[cfg(unix)]
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;

    #[cfg(unix)]
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn protected_folders_match_by_component_under_the_home_folder() {
        let home = Path::new("/Users/dev");
        let under = |name: &str| Some(PathBuf::from("/Users/dev").join(name));
        let cases = [
            ("/Users/dev/Desktop/marsa-helper", under("Desktop")),
            (
                "/Users/dev/Desktop/marsa/target/release/marsa-helper",
                under("Desktop"),
            ),
            ("/Users/dev/Documents/x/marsa-helper", under("Documents")),
            ("/Users/dev/Downloads/marsa-helper", under("Downloads")),
            // Without ASCII case, and as the path writes the folder.
            ("/Users/dev/desktop/marsa-helper", under("desktop")),
            ("/Users/dev/DOWNLOADS/x/marsa-helper", under("DOWNLOADS")),
            ("/Users/dev/documents2/marsa-helper", None),
            ("/Users/dev/Desktop2/marsa-helper", None),
            ("/Users/dev/DesktopX/Desktop/marsa-helper", None),
            (
                "/Users/dev/Library/Application Support/dev.marsa.Marsa/bin/marsa-helper",
                None,
            ),
            ("/Users/other/Desktop/marsa-helper", None),
            ("/Desktop/marsa-helper", None),
        ];
        for (helper, want) in cases {
            let helper = Path::new(helper);
            assert_eq!(
                protected_folder(helper, Some(home)),
                want,
                "{}",
                helper.display()
            );
            // Without a home folder nothing can be known, so nothing matches.
            assert_eq!(protected_folder(helper, None), None, "{}", helper.display());
        }
    }

    #[test]
    fn no_op_means_no_prompt() {
        let dir = Dir::new(false);
        let fake = Fake::new(|program| panic!("{program} ran"));
        let setup = dir.setup(&fake);
        let stop = CancellationToken::new();
        assert_eq!(
            setup.apply(Vec::new(), Platform::DarwinArm64, &stop),
            Ok(Vec::new())
        );
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_missing_helper_is_setup_helper_missing_and_no_prompt() {
        let dir = Dir::new(true);
        let fake = Fake::scripted(netstat_free(), || panic!("osascript ran"));
        let stop = CancellationToken::new();
        let missing = dir.dir.path().join("nowhere").join("marsa-helper");
        for helper in [None, Some(missing), Some(PathBuf::from("marsa-helper"))] {
            let setup = Setup::with(
                fake.clone(),
                Some(Platform::DarwinArm64),
                helper,
                Some(dir.home()),
            );
            let report = setup.run(
                dir.dir.path(),
                CaddyInput::default(),
                Platform::DarwinArm64,
                dir.caddy.as_ref(),
                Some(&[]),
                None,
                &stop,
            );
            assert_eq!(error_code(&report), Some("setup_helper_missing"));
            assert!(report.results.is_empty());
        }
        // No prompt, and no trust dialog after a setup that stopped.
        assert_eq!(fake.programs(), [NETSTAT, NETSTAT, NETSTAT]);
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_others_could_change_is_setup_helper_unsafe_and_no_prompt() {
        let file_writable = "the file is writable by group or others";
        let folder_writable = "the folder is writable by group or others";
        let not_regular = "the file is not a regular file";
        /// What a case does to the helper file at the path.
        type Change = fn(&Path);
        let cases: [(&str, Change, &str); 6] = [
            (
                "group may write the file",
                |h| set_mode(h, 0o775),
                file_writable,
            ),
            (
                "others may write the file",
                |h| set_mode(h, 0o757),
                file_writable,
            ),
            (
                "group may write the folder",
                |h| set_mode(h.parent().unwrap(), 0o775),
                folder_writable,
            ),
            (
                "others may write the folder",
                |h| set_mode(h.parent().unwrap(), 0o777),
                folder_writable,
            ),
            (
                "a symlink",
                |h| {
                    let real = h.parent().unwrap().join("real-helper");
                    fs::rename(h, &real).unwrap();
                    std::os::unix::fs::symlink(&real, h).unwrap();
                },
                not_regular,
            ),
            (
                "a folder",
                |h| {
                    fs::remove_file(h).unwrap();
                    fs::create_dir(h).unwrap();
                },
                not_regular,
            ),
        ];
        for (name, change, reason) in cases {
            let dir = Dir::new(true);
            assert_eq!(check_helper(&dir.helper()), Ok(()), "{name}");
            change(&dir.helper());
            assert_eq!(
                check_helper(&dir.helper()),
                Err(HelperFault::Unsafe(reason)),
                "{name}"
            );

            let fake = Fake::scripted(netstat_free(), || panic!("osascript ran"));
            let report = dir.run(&fake);
            let error = report.error.as_ref().expect(name);
            assert_eq!(error.code, "setup_helper_unsafe", "{name}");
            assert!(error.message.contains(reason), "{name}: {}", error.message);
            assert!(report.results.is_empty(), "{name}");
            assert_eq!(fake.programs(), [NETSTAT], "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_current_user_is_the_owner_of_a_new_file() {
        use std::os::unix::fs::MetadataExt;

        // A file owned by another user needs root to make, so the owner
        // check is tested through the user it compares with.
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("mine");
        fs::write(&file, "").unwrap();
        assert_eq!(current_uid().unwrap(), fs::metadata(&file).unwrap().uid());
    }
}
