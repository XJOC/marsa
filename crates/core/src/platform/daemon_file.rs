//! `run/daemon.json` and the daemon token (architecture.md section 5,
//! "Security"). Task M3-03.
//!
//! The daemon writes the file once it knows its port. The CLI and the window
//! read it to find the port and the bearer token. The token is a secret: it has
//! no `Display` and no `Serialize`, its `Debug` is redacted, and no error
//! message holds it.
//!
//! On Unix the file is mode `0600` inside a `0700` folder. Windows has no mode
//! bits: the file sits under `%LOCALAPPDATA%` and inherits its ACL, which gives
//! access to the user only. Task S-08c checks this on a real Windows machine.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "daemon.json";
const TEMP_NAME: &str = "daemon.json.tmp";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// The daemon's bearer token: 32 random bytes, sent as 64 lowercase hex
/// characters. A new one is made at every start.
pub struct Token([u8; 32]);

impl Token {
    /// A new token from the OS random source.
    ///
    /// # Errors
    ///
    /// `random_unavailable` if the OS gives no random bytes.
    pub fn generate() -> Result<Self, DaemonFileError> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(DaemonFileError::RandomUnavailable)?;
        Ok(Self(bytes))
    }

    /// Whether `presented` is this token in hex. A well-formed `presented` is
    /// compared on every byte, so the time taken does not show how many
    /// leading bytes were right. `==` stops at the first difference, which
    /// leaks that.
    pub fn verify(&self, presented: &str) -> bool {
        // Decoding branches on `presented` only, never on the token, and the
        // length is no secret: it is always 64.
        let Some(presented) = Token::from_hex(presented) else {
            return false;
        };
        let mut diff = 0_u8;
        for (a, b) in self.0.iter().zip(presented.0) {
            // `black_box` keeps the optimizer from adding an early exit.
            diff = std::hint::black_box(diff | (a ^ b));
        }
        diff == 0
    }

    /// The `Authorization` header value, `Bearer <hex>`. The only way to read
    /// the token: for the client, which sends it to the daemon.
    pub fn bearer_header(&self) -> String {
        format!("Bearer {}", self.hex_string())
    }

    fn hex_string(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
        }
        out
    }

    /// Reads 64 lowercase hex characters, the form `hex_string` writes.
    fn from_hex(text: &str) -> Option<Self> {
        let text = text.as_bytes();
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0_u8; 32];
        for (byte, &[high, low]) in bytes.iter_mut().zip(text.as_chunks::<2>().0) {
            *byte = (hex_value(high)? << 4) | hex_value(low)?;
        }
        Some(Self(bytes))
    }
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// What `run/daemon.json` holds. `Debug` is safe to log because the token
/// prints redacted. There is no `Serialize`: only [`write_daemon_file`] turns
/// it into JSON, so the token cannot slip into an API response.
#[derive(Debug)]
pub struct DaemonFile {
    pub pid: u32,
    pub port: u16,
    pub token: Token,
    /// The daemon's `CARGO_PKG_VERSION`.
    pub app_version: String,
}

/// The JSON form of [`DaemonFile`]. Private, and without `Debug`.
#[derive(Serialize, Deserialize)]
struct OnDisk {
    pid: u32,
    port: u16,
    token: String,
    app_version: String,
}

/// Why `run/daemon.json` could not be read or written. No message holds the
/// token or any other content of the file.
#[derive(Debug, thiserror::Error)]
pub enum DaemonFileError {
    #[error("no daemon file at {}", .path.display())]
    Missing { path: PathBuf },

    /// `reason` is fixed text, never the file's content.
    #[error("{} is not a valid daemon file: {reason}", .path.display())]
    Invalid { path: PathBuf, reason: &'static str },

    #[error("cannot read {}", .path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{} can be read by other users (mode {mode:03o}); it must be 600", .path.display())]
    Insecure { path: PathBuf, mode: u32 },

    #[error("cannot write {}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("the OS random source failed")]
    RandomUnavailable(#[source] getrandom::Error),
}

impl DaemonFileError {
    /// Stable error name for the UI. Never changes once released. A file that
    /// exists but cannot be read counts as `daemon_file_invalid`.
    pub fn code(&self) -> &'static str {
        match self {
            DaemonFileError::Missing { .. } => "daemon_file_missing",
            DaemonFileError::Invalid { .. } | DaemonFileError::Unreadable { .. } => {
                "daemon_file_invalid"
            }
            DaemonFileError::Insecure { .. } => "daemon_file_insecure",
            DaemonFileError::Write { .. } => "daemon_file_write",
            DaemonFileError::RandomUnavailable(_) => "random_unavailable",
        }
    }
}

/// The path of `daemon.json` inside `run_dir`.
pub fn daemon_file_path(run_dir: &Path) -> PathBuf {
    run_dir.join(FILE_NAME)
}

/// Writes `run_dir/daemon.json` atomically: a temp file in `run_dir`, created
/// with mode `0600` from the start, flushed to disk, then renamed over the old
/// file. A reader sees the old file or the new one, never half of one.
///
/// Only the daemon calls this, while it holds the `run/daemon.lock` lock.
///
/// # Errors
///
/// `daemon_file_write` if the folder or the file cannot be written.
pub fn write_daemon_file(run_dir: &Path, file: &DaemonFile) -> Result<(), DaemonFileError> {
    let path = daemon_file_path(run_dir);
    let fail = |source| DaemonFileError::Write {
        path: path.clone(),
        source,
    };
    create_private_dir(run_dir).map_err(fail)?;
    let on_disk = OnDisk {
        pid: file.pid,
        port: file.port,
        token: file.token.hex_string(),
        app_version: file.app_version.clone(),
    };
    let mut body =
        serde_json::to_vec_pretty(&on_disk).map_err(|err| fail(io::Error::other(err)))?;
    body.push(b'\n');
    let temp = run_dir.join(TEMP_NAME);
    let result = write_new(&temp, &body).and_then(|()| fs::rename(&temp, &path));
    if result.is_err() {
        // Best effort: the write already failed, and that is the error to report.
        let _ = fs::remove_file(&temp);
    }
    result.map_err(fail)
}

/// Reads `run_dir/daemon.json`.
///
/// # Errors
///
/// `daemon_file_missing` if there is no file, `daemon_file_insecure` if other
/// users may read it (Unix), and `daemon_file_invalid` if it cannot be read or
/// is not a daemon file.
pub fn read_daemon_file(run_dir: &Path) -> Result<DaemonFile, DaemonFileError> {
    let path = daemon_file_path(run_dir);
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(DaemonFileError::Missing { path });
        }
        Err(source) => return Err(DaemonFileError::Unreadable { path, source }),
    };
    let invalid = |reason| DaemonFileError::Invalid {
        path: path.clone(),
        reason,
    };
    let unreadable = |source| DaemonFileError::Unreadable {
        path: path.clone(),
        source,
    };
    // The metadata of the open file, not of the path, so the check and the
    // read see the same file.
    check_private(&path, &file.metadata().map_err(unreadable)?)?;

    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|err| match err.kind() {
            io::ErrorKind::InvalidData => invalid("not UTF-8 text"),
            _ => unreadable(err),
        })?;
    // The `serde_json` message can quote a value from the file, so it is dropped.
    let on_disk: OnDisk =
        serde_json::from_str(&text).map_err(|_| invalid("not the expected JSON"))?;
    let token = Token::from_hex(&on_disk.token)
        .ok_or_else(|| invalid("the token is not 64 lowercase hex characters"))?;
    Ok(DaemonFile {
        pid: on_disk.pid,
        port: on_disk.port,
        token,
        app_version: on_disk.app_version,
    })
}

/// Deletes `run_dir/daemon.json` at a clean shutdown. No file is not an error.
///
/// # Errors
///
/// `daemon_file_write` if the file exists and cannot be deleted.
pub fn remove_daemon_file(run_dir: &Path) -> Result<(), DaemonFileError> {
    let path = daemon_file_path(run_dir);
    match fs::remove_file(&path) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => {
            Err(DaemonFileError::Write { path, source })
        }
        _ => Ok(()),
    }
}

/// Creates the temp file and writes `body` to disk. `create_new` refuses a
/// file or a symlink that is already there, and on Unix the mode is `0600`
/// from the first byte, with no `chmod` after the write.
fn write_new(path: &Path, body: &[u8]) -> io::Result<()> {
    let mut file = match create_new(path) {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            // Left by a daemon that stopped mid-write. The lock means no other
            // daemon writes here now.
            fs::remove_file(path)?;
            create_new(path)?
        }
        other => other?,
    };
    file.write_all(body)?;
    file.sync_all()
}

fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Creates `dir` (and its parents) for the owner only. An existing `dir` that
/// others may open is closed to them.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Windows: the folder inherits the user-only ACL of `%LOCALAPPDATA%`.
#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn check_private(path: &Path, metadata: &fs::Metadata) -> Result<(), DaemonFileError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(DaemonFileError::Insecure {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

/// Windows: the file inherits the user-only ACL of `%LOCALAPPDATA%` (S-08c).
#[cfg(not(unix))]
fn check_private(_path: &Path, _metadata: &fs::Metadata) -> Result<(), DaemonFileError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const KNOWN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn sample() -> DaemonFile {
        DaemonFile {
            pid: 4242,
            port: 51234,
            token: Token::generate().unwrap(),
            app_version: "0.0.0".into(),
        }
    }

    /// Writes `text` as `daemon.json`, owner-only, so the mode check passes.
    fn put(run: &Path, text: &str) {
        fs::write(daemon_file_path(run), text).unwrap();
        #[cfg(unix)]
        set_mode(&daemon_file_path(run), 0o600);
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn tokens_are_64_lowercase_hex_and_differ() {
        let a = Token::generate().unwrap();
        let b = Token::generate().unwrap();
        for token in [&a, &b] {
            let hex = token.hex_string();
            assert_eq!(hex.len(), 64);
            assert!(hex.bytes().all(|c| HEX_DIGITS.contains(&c)), "{hex}");
            assert_eq!(token.bearer_header(), format!("Bearer {hex}"));
        }
        assert_ne!(a.hex_string(), b.hex_string());
    }

    #[test]
    fn debug_never_shows_the_token() {
        let file = sample();
        let hex = file.token.hex_string();
        assert_eq!(format!("{:?}", file.token), "Token(<redacted>)");
        for debug in [format!("{file:?}"), format!("{file:#?}")] {
            assert!(!debug.contains(&hex), "{debug}");
            assert!(debug.contains("51234"), "{debug}");
        }
    }

    #[test]
    fn verify_accepts_only_the_exact_token() {
        let token = Token::from_hex(KNOWN).unwrap();
        assert_eq!(token.hex_string(), KNOWN);
        assert!(token.verify(KNOWN));
        let last_changed = format!("{}0", &KNOWN[..63]);
        let other = Token::generate().unwrap().hex_string();
        for bad in [
            "",
            &KNOWN[..63],
            &KNOWN[..32],
            &format!("{KNOWN}0"),
            &format!(" {KNOWN}"),
            &KNOWN.to_uppercase(),
            &last_changed,
            &other,
        ] {
            assert!(!token.verify(bad), "{bad:?}");
        }
    }

    #[test]
    fn write_then_read_gives_the_same_file() {
        let dir = TempDir::new().unwrap();
        let run = dir.path().join("run");
        let file = sample();
        write_daemon_file(&run, &file).unwrap();

        let text = fs::read_to_string(daemon_file_path(&run)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "pid": 4242,
                "port": 51234,
                "token": file.token.hex_string(),
                "app_version": "0.0.0",
            })
        );
        let back = read_daemon_file(&run).unwrap();
        assert_eq!((back.pid, back.port), (4242, 51234));
        assert_eq!(back.app_version, "0.0.0");
        assert!(back.token.verify(&file.token.hex_string()));
    }

    #[test]
    fn write_replaces_the_old_file_and_a_stale_temp_file() {
        let dir = TempDir::new().unwrap();
        write_daemon_file(dir.path(), &sample()).unwrap();
        fs::write(dir.path().join(TEMP_NAME), "left by a crash").unwrap();
        let mut newer = sample();
        newer.port = 40000;
        write_daemon_file(dir.path(), &newer).unwrap();
        assert_eq!(read_daemon_file(dir.path()).unwrap().port, 40000);
        assert!(!dir.path().join(TEMP_NAME).exists());
    }

    #[test]
    fn read_rejects_a_missing_or_bad_file_without_showing_it() {
        let dir = TempDir::new().unwrap();
        let err = read_daemon_file(dir.path()).unwrap_err();
        assert_eq!(err.code(), "daemon_file_missing", "{err}");

        let good = Token::generate().unwrap().hex_string();
        for text in [
            "not json".to_owned(),
            format!(r#"{{"pid": 1, "port": "{good}", "token": "{good}", "app_version": "0"}}"#),
            r#"{"pid": 1, "port": 2, "token": "abc", "app_version": "0"}"#.to_owned(),
            format!(
                r#"{{"pid": 1, "port": 2, "token": "{}", "app_version": "0"}}"#,
                KNOWN.to_uppercase()
            ),
            format!(r#"{{"pid": 1, "port": 2, "token": "{good}"}}"#),
        ] {
            put(dir.path(), &text);
            let err = read_daemon_file(dir.path()).unwrap_err();
            assert_eq!(err.code(), "daemon_file_invalid", "{err}");
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(&good), "{shown}");
        }
    }

    #[test]
    fn remove_deletes_the_file_and_accepts_no_file() {
        let dir = TempDir::new().unwrap();
        write_daemon_file(dir.path(), &sample()).unwrap();
        remove_daemon_file(dir.path()).unwrap();
        assert!(!daemon_file_path(dir.path()).exists());
        remove_daemon_file(dir.path()).unwrap();
    }

    #[test]
    fn write_into_a_file_instead_of_a_folder_fails() {
        let dir = TempDir::new().unwrap();
        let run = dir.path().join("run");
        fs::write(&run, "").unwrap();
        let err = write_daemon_file(&run, &sample()).unwrap_err();
        assert_eq!(err.code(), "daemon_file_write");
    }

    #[test]
    fn random_failure_has_its_code() {
        let err = DaemonFileError::RandomUnavailable(getrandom::Error::UNSUPPORTED);
        assert_eq!(err.code(), "random_unavailable");
    }

    #[cfg(unix)]
    #[test]
    fn unix_file_is_0600_in_a_0700_folder() {
        let dir = TempDir::new().unwrap();
        let run = dir.path().join("run");
        write_daemon_file(&run, &sample()).unwrap();
        assert_eq!(mode(&daemon_file_path(&run)), 0o600);
        assert_eq!(mode(&run), 0o700);

        for open in [0o644, 0o640, 0o604] {
            set_mode(&daemon_file_path(&run), open);
            let err = read_daemon_file(&run).unwrap_err();
            assert_eq!(err.code(), "daemon_file_insecure", "{err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_existing_open_folder_is_closed() {
        let dir = TempDir::new().unwrap();
        let run = dir.path().join("run");
        fs::create_dir(&run).unwrap();
        set_mode(&run, 0o755);
        write_daemon_file(&run, &sample()).unwrap();
        assert_eq!(mode(&run), 0o700);
    }
}
