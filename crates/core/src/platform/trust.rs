//! The user's trust in Marsa's CA on macOS, as far as a client needs it: the
//! files of the record, and whether `security` trusts the root. The daemon
//! writes the trust (`install`, `remove`); the CLI only reads it.

use crate::platform::ports::CHECK_LIMIT;
use crate::platform::runner::Runner;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// The tool, by its absolute path (AGENTS.md section 6).
pub const SECURITY: &str = "/usr/bin/security";

/// The trust folder under the data folder, and the two files in it.
const TRUST_DIR: &str = "trust";
const RECORD: &str = "user-trusted.pem";
const PENDING: &str = "pending.pem";

/// `<data>/trust/`: the record of the certificate this user trusted, and
/// the checked copy `security` gets during setup (see `daemon::setup_api::user_trust`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustFiles {
    dir: PathBuf,
}

impl TrustFiles {
    pub fn in_dir(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(TRUST_DIR),
        }
    }

    /// The folder itself: `<data>/trust`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `user-trusted.pem`: the certificate this user trusted.
    pub fn record(&self) -> PathBuf {
        self.dir.join(RECORD)
    }

    /// `pending.pem`: the checked certificate, while setup trusts it.
    pub fn pending(&self) -> PathBuf {
        self.dir.join(PENDING)
    }
}

/// `<data_dir>/trust/user-trusted.pem`: the certificate this user trusted.
pub fn record_path(data_dir: &Path) -> PathBuf {
    TrustFiles::in_dir(data_dir).record()
}

/// For `marsa doctor` whether the certificate at `ca_root` is
/// trusted as a root now, by the same `verify-cert` as the install. `None`
/// when that is not known: the path is not absolute, or `security` could not
/// run or was stopped by a signal. Changes nothing and shows no dialog.
pub fn trusted(runner: &dyn Runner, ca_root: &Path, stop: &CancellationToken) -> Option<bool> {
    if !ca_root.is_absolute() {
        return None;
    }
    match runner.run(SECURITY, &verify_args(ca_root), CHECK_LIMIT, stop) {
        Ok(output) => output.code.map(|_| output.success()),
        Err(err) => {
            tracing::debug!(error = %err, "cannot run security verify-cert");
            None
        }
    }
}

/// `-L`: no fetch of missing certificates from the net. `-l`: the one
/// certificate given is a CA, which `verify-cert` refuses otherwise.
pub fn verify_args(file: &Path) -> Vec<OsString> {
    vec![
        OsString::from("verify-cert"),
        OsString::from("-c"),
        file.as_os_str().to_owned(),
        OsString::from("-L"),
        OsString::from("-l"),
    ]
}
