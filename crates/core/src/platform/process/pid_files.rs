//! The pid files in `run/`, and the data folder's instance id.

use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::Path;

use crate::orphan::{PidRecord, is_instance_id};

/// The longest `run/caddy.pid` that is read.
const PID_FILE_MAX: u64 = 4096;

/// Writes `run/caddy.pid` whole: a temp file next to it, then a rename, so a
/// reader never sees half a file. On Unix the file is `0600` from the start.
///
/// # Errors
///
/// The folder or the file cannot be written.
pub fn write_pid_file(path: &Path, record: &PidRecord) -> io::Result<()> {
    let mut body = serde_json::to_vec(record).map_err(io::Error::other)?;
    body.push(b'\n');
    crate::fs::write_atomic(path, &body, Some(0o600))
}

/// The instance id of a data folder, kept in `path` (`caddy/instance-id`):
/// read when the file holds one, else 16 random bytes in hex, written once,
/// `0600`, through a temp file and a rename. Only a file whose content is no
/// id is replaced: a Caddy tagged with the lost id then counts as another
/// Marsa's, which is never touched. A failed read is an error, so a valid id
/// is never replaced because of it.
///
/// # Errors
///
/// The file cannot be read or written, or the OS gives no random bytes.
pub fn instance_id(path: &Path) -> io::Result<String> {
    match File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            // Longer than an id is not an id: no need to read it all.
            file.take(64).read_to_end(&mut bytes)?;
            let text = std::str::from_utf8(&bytes).unwrap_or_default();
            let text = text.trim_end_matches(['\r', '\n']);
            if is_instance_id(text) {
                return Ok(text.to_owned());
            }
            tracing::warn!(path = %path.display(), "the instance id file holds no id; making a new one");
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|err| io::Error::other(err.to_string()))?;
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    crate::fs::write_atomic(path, format!("{id}\n").as_bytes(), Some(0o600))?;
    Ok(id)
}

/// Reads a pid file (`run/caddy.pid`, `run/php-fpm-<minor>.pid`). `None` if
/// it is missing, not a regular file, too long, or not a pid record: then it
/// names no pid. A link, even to a regular file, is refused, and so is a FIFO,
/// whose `open` would wait for a writer without end.
pub fn read_pid_file(path: &Path) -> Option<PidRecord> {
    serde_json::from_str(&read_small_file(path)?).ok()
}

/// Reads a small pid file as text, with the checks of [`read_pid_file`]:
/// `None` if it is missing, a link, not a regular file, longer than 4096
/// bytes, or not UTF-8, and on Unix if the file opened is not the one
/// checked (its device and inode). For a server's own pid file, such as
/// PostgreSQL's `postmaster.pid` (task M6-03).
pub fn read_small_file(path: &Path) -> Option<String> {
    let before = fs::symlink_metadata(path).ok()?;
    if !before.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    // Checked again on the open file: the path may have changed in between,
    // for example to a link to another file.
    let opened = file.metadata().ok()?;
    if !opened.is_file() || !same_file(&before, &opened) {
        return None;
    }
    let mut text = String::new();
    file.take(PID_FILE_MAX + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > PID_FILE_MAX {
        return None;
    }
    Some(text)
}

/// Whether two looks saw the same file: the same device and inode. Always
/// `true` on Windows, which has no stable file id; the checks of the type
/// stay.
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    match (crate::fs::file_id(a), crate::fs::file_id(b)) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Deletes `run/caddy.pid`. No file is not an error.
///
/// # Errors
///
/// The file exists and cannot be deleted.
pub fn remove_pid_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use super::super::at;

    #[test]
    fn the_instance_id_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caddy").join("instance-id");
        let id = instance_id(&path).unwrap();
        assert!(is_instance_id(&id), "{id}");
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{id}\n"));
        assert_eq!(instance_id(&path).unwrap(), id, "read, not made again");
        // Mode bits exist on Unix only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Another folder gets another id.
        let other = tempfile::tempdir().unwrap();
        let other = instance_id(&other.path().join("instance-id")).unwrap();
        assert_ne!(other, id);
    }

    #[test]
    fn a_file_without_an_id_gets_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance-id");
        for bad in [&b"not an id\n"[..], &[0xff; 32][..]] {
            fs::write(&path, bad).unwrap();
            let id = instance_id(&path).unwrap();
            assert!(is_instance_id(&id), "{id}");
            assert_eq!(instance_id(&path).unwrap(), id);
        }
    }

    #[test]
    fn a_failed_read_is_an_error_and_replaces_nothing() {
        // A folder where the file should be: it opens on Unix and fails to
        // read, and fails to open on Windows. Either way nothing is written.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance-id");
        fs::create_dir(&path).unwrap();
        assert!(instance_id(&path).is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn the_pid_file_round_trips_and_replaces_a_stale_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("caddy.pid");
        assert_eq!(read_pid_file(&path), None);
        let record = PidRecord {
            pid: 42,
            exe: PathBuf::from("/data/bin/caddy"),
            started: at("Wed Nov  4 03:31:27 2026"),
        };
        write_pid_file(&path, &record).unwrap();
        fs::write(dir.path().join("run").join(".caddy.pid.tmp"), "half").unwrap();
        write_pid_file(&path, &record).unwrap();
        assert_eq!(read_pid_file(&path), Some(record));
        let names: Vec<_> = fs::read_dir(dir.path().join("run"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["caddy.pid"], "no temp file left");
        // Mode bits exist on Unix only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        remove_pid_file(&path).unwrap();
        remove_pid_file(&path).unwrap();
        assert_eq!(read_pid_file(&path), None);
    }

    // FIFOs and symlinks to them are a Unix matter.
    #[cfg(unix)]
    #[test]
    fn a_fifo_or_a_link_at_the_pid_file_path_names_no_pid_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("caddy.pid");
        let made = std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let real = dir.path().join("real.pid");
        let record = PidRecord {
            pid: 42,
            exe: PathBuf::from("/data/bin/caddy"),
            started: at("Wed Nov  4 03:31:27 2026"),
        };
        write_pid_file(&real, &record).unwrap();
        let to_real = dir.path().join("to-real.pid");
        let to_fifo = dir.path().join("to-fifo.pid");
        std::os::unix::fs::symlink(&real, &to_real).unwrap();
        std::os::unix::fs::symlink(&fifo, &to_fifo).unwrap();

        // On a thread: a `read_pid_file` that opens the FIFO never returns,
        // and must not hang the test run.
        let (sender, answers) = std::sync::mpsc::channel();
        let paths = [fifo, to_fifo, to_real];
        std::thread::spawn(move || {
            for path in paths {
                let _ = sender.send(read_pid_file(&path));
            }
        });
        for _ in 0..3 {
            let answer = answers.recv_timeout(std::time::Duration::from_secs(10));
            assert_eq!(answer, Ok(None));
        }
        assert_eq!(read_pid_file(&real), Some(record));
    }

    #[test]
    fn a_pid_file_that_is_not_a_record_names_no_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caddy.pid");
        let long = format!(
            "{{\"pid\":42,\"exe\":\"/x\",\"started\":\"{}\"}}",
            "x".repeat(5000)
        );
        for text in ["", "42", "{\"pid\":42}", &long] {
            fs::write(&path, text).unwrap();
            assert_eq!(read_pid_file(&path), None, "{text:?}");
        }
    }
}
