//! Every file and process access of the helper goes through [`System`].
//!
//! The ops get a `&impl System`, so tests run them against [`FakeSystem`]
//! (like `Http::fake()` in Laravel) and never touch the real machine.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// The only `PATH` a child process sees. The helper runs absolute paths, so
/// this only stops a child from finding programs through the caller's `PATH`.
const SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The output of a program that finished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    /// `None` if a signal stopped the program.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The file and process calls the ops may make.
// The op handlers (tasks M4-07 to M4-09) call these methods; until then only
// the tests do.
#[allow(dead_code)]
pub trait System {
    /// The whole file, or an error if it is larger than `max` bytes or is not
    /// a regular file.
    fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>>;

    /// Whether `path` exists. A symlink counts even if its target does not.
    fn exists(&self, path: &Path) -> bool;

    /// Writes a temp file in the same folder, syncs it, then renames it over
    /// `path`. Creates the parent folder with mode `0755` if it is missing.
    /// `mode` is the Unix file mode; Windows ignores it.
    fn write_atomic(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()>;

    /// Deletes a file. A missing file counts as success.
    fn remove(&self, path: &Path) -> io::Result<()>;

    /// Runs `program` with `args`, never through a shell. `&'static str`
    /// means a constant in the code, like `/sbin/pfctl`: it cannot come from
    /// the request.
    fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput>;
}

/// The real disk and processes.
pub struct RealSystem;

impl System for RealSystem {
    fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
        // Checked before `open` too, because opening a FIFO waits for a writer.
        regular_within(&fs::metadata(path)?, max)?;
        let file = File::open(path)?;
        regular_within(&file.metadata()?, max)?;
        // The file can grow after the check, so the read stops at `max + 1`.
        let mut bytes = Vec::new();
        file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max {
            return Err(too_large(max));
        }
        Ok(bytes)
    }

    fn exists(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn write_atomic(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path has no folder or no file name",
            ));
        };
        create_dir(dir)?;
        let mut tmp_name = OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(".marsa-tmp-{}", std::process::id()));
        let tmp = dir.join(tmp_name);

        let mut options = OpenOptions::new();
        // `create_new`: never write through a file or symlink already there.
        options.write(true).create_new(true);
        // File modes exist on Unix only. On Windows the file gets the ACL of
        // its folder.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
        // From here on the temp file is ours, so it is removed on any error.
        let file = options.open(&tmp)?;
        let result = fill(file, bytes, mode).and_then(|()| fs::rename(&tmp, path));
        if result.is_err() {
            // Best effort: the first error is the one to report.
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
        if !Path::new(program).is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the program is not an absolute path",
            ));
        }
        let output = Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", SAFE_PATH)
            .stdin(Stdio::null())
            .output()?;
        Ok(RunOutput {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// Sets the mode, writes, and syncs, so a power cut cannot leave an empty file
/// after the rename. The file is closed when this returns, before the rename.
fn fill(mut file: File, bytes: &[u8], mode: u32) -> io::Result<()> {
    set_mode(&file, mode)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn regular_within(meta: &fs::Metadata, max: u64) -> io::Result<()> {
    if !meta.is_file() {
        let reason = "not a regular file";
        return Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
    }
    if meta.len() > max {
        return Err(too_large(max));
    }
    Ok(())
}

fn too_large(max: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the file is larger than {max} bytes"),
    )
}

/// Creates `dir` and the folders above it. Only `dir` itself is set to `0755`
/// against the umask; folders that exist keep their mode.
fn create_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    // Folder modes exist on Unix only.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o755);
    builder.create(dir)?;
    // Again after `create`, because the umask may have removed bits.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Sets the mode again after `open`, because the umask may have removed bits.
/// Through the open file, not the path, so it cannot follow a swapped link.
// File modes exist on Unix only.
#[cfg(unix)]
fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

/// Windows has no file modes. The file gets the ACL of its folder.
#[cfg(not(unix))]
fn set_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
pub use fake::FakeSystem;

#[cfg(test)]
mod fake {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{RunOutput, System, too_large};

    /// Files and commands in memory, for tests.
    #[derive(Default)]
    pub struct FakeSystem {
        /// Path to content and mode.
        pub files: RefCell<BTreeMap<PathBuf, (Vec<u8>, u32)>>,
        /// Every `run` call: the program and its arguments.
        pub log: RefCell<Vec<(String, Vec<String>)>>,
        /// The output of each program. A program without one fails as not found.
        pub outputs: BTreeMap<&'static str, RunOutput>,
    }

    impl System for FakeSystem {
        fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
            match self.files.borrow().get(path) {
                Some((bytes, _)) if bytes.len() as u64 > max => Err(too_large(max)),
                Some((bytes, _)) => Ok(bytes.clone()),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }

        fn exists(&self, path: &Path) -> bool {
            self.files.borrow().contains_key(path)
        }

        fn write_atomic(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
            let entry = (bytes.to_vec(), mode);
            self.files.borrow_mut().insert(path.to_path_buf(), entry);
            Ok(())
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            self.files.borrow_mut().remove(path);
            Ok(())
        }

        fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
            let args = args.iter().map(|arg| (*arg).to_owned()).collect();
            self.log.borrow_mut().push((program.to_owned(), args));
            let output = self.outputs.get(program).cloned();
            output.ok_or_else(|| io::ErrorKind::NotFound.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_atomic_creates_the_folder_and_leaves_no_temp_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("a").join("b");
        let path = dir.join("rules.conf");
        RealSystem.write_atomic(&path, b"one", 0o644).unwrap();
        RealSystem.write_atomic(&path, b"two", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["rules.conf"]);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_sets_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("new");
        let path = dir.join("token");
        RealSystem.write_atomic(&path, b"x", 0o644).unwrap();
        assert_eq!(mode(&path), 0o644);
        assert_eq!(mode(&dir), 0o755);
        RealSystem.write_atomic(&path, b"x", 0o600).unwrap();
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn read_refuses_large_files_and_folders() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("root.crt");
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(RealSystem.read(&path, 10).unwrap(), b"0123456789");
        let err = RealSystem.read(&path, 9).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let err = RealSystem.read(tmp.path(), 10).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn remove_and_exists() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("gone");
        RealSystem.remove(&path).unwrap();
        fs::write(&path, b"").unwrap();
        assert!(RealSystem.exists(&path));
        RealSystem.remove(&path).unwrap();
        assert!(!RealSystem.exists(&path));
    }

    #[test]
    fn run_refuses_a_relative_program() {
        let err = RealSystem.run("pfctl", &["-E"]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// `/usr/bin/env` prints its environment: only the fixed `PATH` is left.
    #[cfg(unix)]
    #[test]
    fn run_clears_the_environment() {
        let output = RealSystem.run("/usr/bin/env", &[]).unwrap();
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, format!("PATH={SAFE_PATH}\n").as_bytes());
    }

    #[test]
    fn fake_system_keeps_files_and_logs_commands() {
        let mut fake = FakeSystem::default();
        fake.outputs.insert("/sbin/pfctl", RunOutput::default());
        let path = Path::new("/etc/resolver/test");
        fake.write_atomic(path, b"port 18053", 0o644).unwrap();
        assert!(fake.exists(path));
        assert!(fake.read(path, 4).is_err());
        assert_eq!(fake.read(path, 10).unwrap(), b"port 18053");
        fake.remove(path).unwrap();
        assert!(!fake.exists(path));
        assert!(fake.run("/sbin/pfctl", &["-E"]).is_ok());
        assert!(fake.run("/usr/bin/true", &[]).is_err());
        let log = fake.log.borrow();
        assert_eq!(log[0], ("/sbin/pfctl".to_owned(), vec!["-E".to_owned()]));
        assert_eq!(log.len(), 2);
    }
}
