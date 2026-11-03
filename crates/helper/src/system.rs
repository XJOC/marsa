//! Every file and process access of the helper goes through [`System`].
//!
//! The ops get a `&impl System`, so tests run them against [`FakeSystem`]
//! (like `Http::fake()` in Laravel) and never touch the real machine.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The only `PATH` a child process sees. The helper runs absolute paths, so
/// this only stops a child from finding programs through the caller's `PATH`.
const SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
/// The mode of a folder that `write_atomic` creates.
const DIR_MODE: u32 = 0o755;

/// The output of a program that finished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    /// `None` if a signal stopped the program.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The file and process calls the ops may make.
///
/// Precondition for every path: it is a constant in the helper's code, in a
/// folder that only root can write. The checks below look at the path and
/// then use it; that is safe only because the user cannot change such a
/// folder between the two steps.
pub trait System {
    /// The whole file, or an error if it is larger than `max` bytes or is not
    /// a regular file. A missing file is an error of kind `NotFound`.
    fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>>;

    /// Whether `path` exists. A symlink counts even if its target does not.
    fn exists(&self, path: &Path) -> bool;

    /// Writes a temp file in the same folder, syncs it, renames it over
    /// `path`, then syncs the folder. Creates the folder with mode `0755` if
    /// it is missing. `mode` is the Unix file mode; Windows ignores it.
    fn write_atomic(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()>;

    /// Deletes a file. A missing file counts as success.
    fn remove(&self, path: &Path) -> io::Result<()>;

    /// Runs `program` with `args`, never through a shell. `&'static str`
    /// means a constant in the code, like `/sbin/pfctl`: it cannot come from
    /// the request.
    // `allow`, not `expect`: M4-08 and M4-09 start using this in parallel,
    // and an `expect` would warn as soon as one of them lands.
    #[allow(dead_code)]
    fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput>;

    /// The path of the running helper, for the boot job (task M4-08).
    // `allow`, not `expect`: used by M4-08, which runs in parallel with M4-09.
    #[allow(dead_code)]
    fn current_exe(&self) -> io::Result<PathBuf>;
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
        create_dir(dir, DIR_MODE)?;
        let mut tmp_name = OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(".marsa-tmp-{}", std::process::id()));
        let tmp = dir.join(tmp_name);
        // A temp file left by a crash of an earlier run with the same PID.
        // `create_new` below still refuses anything that appears after this.
        let _ = fs::remove_file(&tmp);

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
        result?;
        sync_dir(dir)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// The environment is empty except `PATH`. The Windows environment
    /// (`SystemRoot`, Windows folders in `PATH`) is set in task M4-19.
    fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
        if !Path::new(program).is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the program is not an absolute path",
            ));
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .env("PATH", SAFE_PATH)
            .stdin(Stdio::null());
        // The child must not start in the caller's folder, which the user
        // controls. `/` is a Unix path; Windows is set in task M4-19.
        #[cfg(unix)]
        command.current_dir("/");
        let output = command.output()?;
        Ok(RunOutput {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn current_exe(&self) -> io::Result<PathBuf> {
        std::env::current_exe()
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

/// Creates `dir` and the folders above it. Only `dir` itself is set to
/// `mode` against the umask; folders that exist keep their mode.
fn create_dir(dir: &Path, mode: u32) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    // Folder modes exist on Unix only.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, mode);
    builder.create(dir)?;
    // Again after `create`, because the umask may have removed bits.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
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

/// Syncs the folder, so the rename itself survives a power cut.
// Unix only: Windows cannot open a folder as a `File`, and NTFS logs renames.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
pub use fake::FakeSystem;

#[cfg(test)]
mod fake {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{RunOutput, System, too_large};

    /// What `current_exe` returns.
    pub const EXE: &str = "/fake/marsa-helper";

    /// Files and commands in memory, for tests.
    #[derive(Default)]
    pub struct FakeSystem {
        /// Path to content and mode.
        pub files: RefCell<BTreeMap<PathBuf, (Vec<u8>, u32)>>,
        /// Every `run` call: the program and its arguments.
        pub log: RefCell<Vec<(String, Vec<String>)>>,
        /// The output of one exact command line. See [`FakeSystem::script`].
        pub outputs: BTreeMap<(&'static str, Vec<String>), RunOutput>,
        /// The output of any other command line of a program. A program with
        /// neither fails as not found.
        pub defaults: BTreeMap<&'static str, RunOutput>,
        /// Paths where `write_atomic` fails and the file stays as it was.
        pub fail_writes: BTreeSet<PathBuf>,
        /// Paths where `remove` fails and the file stays.
        pub fail_removes: BTreeSet<PathBuf>,
        /// Paths where `read` fails with `PermissionDenied`.
        pub fail_reads: BTreeSet<PathBuf>,
        /// Dangling symlinks: `exists` is true, but `read` is `NotFound`,
        /// like a link whose target is gone.
        pub dangling: BTreeSet<PathBuf>,
    }

    impl FakeSystem {
        /// Scripts the output of `program` run with exactly `args`.
        pub fn script(&mut self, program: &'static str, args: &[&str], output: RunOutput) {
            self.outputs.insert((program, strings(args)), output);
        }
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn injected() -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, "injected failure")
    }

    impl System for FakeSystem {
        fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
            if self.fail_reads.contains(path) {
                return Err(injected());
            }
            match self.files.borrow().get(path) {
                Some((bytes, _)) if bytes.len() as u64 > max => Err(too_large(max)),
                Some((bytes, _)) => Ok(bytes.clone()),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }

        fn exists(&self, path: &Path) -> bool {
            self.dangling.contains(path) || self.files.borrow().contains_key(path)
        }

        fn write_atomic(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
            if self.fail_writes.contains(path) {
                return Err(injected());
            }
            let entry = (bytes.to_vec(), mode);
            self.files.borrow_mut().insert(path.to_path_buf(), entry);
            Ok(())
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            if self.fail_removes.contains(path) {
                return Err(injected());
            }
            self.files.borrow_mut().remove(path);
            Ok(())
        }

        fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
            let key = (program, strings(args));
            self.log
                .borrow_mut()
                .push((program.to_owned(), key.1.clone()));
            let output = self
                .outputs
                .get(&key)
                .or_else(|| self.defaults.get(program));
            output
                .cloned()
                .ok_or_else(|| io::ErrorKind::NotFound.into())
        }

        fn current_exe(&self) -> io::Result<PathBuf> {
            Ok(PathBuf::from(EXE))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn names_in(dir: &Path) -> Vec<OsString> {
        let entries = fs::read_dir(dir).unwrap();
        entries.map(|entry| entry.unwrap().file_name()).collect()
    }

    #[test]
    fn write_atomic_creates_the_folder_and_leaves_no_temp_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("a").join("b");
        let path = dir.join("rules.conf");
        RealSystem.write_atomic(&path, b"one", 0o644).unwrap();
        RealSystem.write_atomic(&path, b"two", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(names_in(&dir), ["rules.conf"]);
    }

    #[test]
    fn write_atomic_removes_the_temp_file_when_the_rename_fails() {
        let tmp = TempDir::new().unwrap();
        // A folder in place of the file: the rename cannot replace it.
        let path = tmp.path().join("rules.conf");
        fs::create_dir(&path).unwrap();
        assert!(RealSystem.write_atomic(&path, b"x", 0o644).is_err());
        assert_eq!(names_in(tmp.path()), ["rules.conf"]);
    }

    /// Modes with group write: a usual umask (`022`) removes that bit, so these
    /// pass only if the mode is set again after `open` and `create`.
    #[cfg(unix)]
    #[test]
    fn modes_survive_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("new").join("rules.conf");
        RealSystem.write_atomic(&path, b"x", 0o664).unwrap();
        assert_eq!(mode(&path), 0o664);
        assert_eq!(mode(&tmp.path().join("new")), DIR_MODE);
        RealSystem.write_atomic(&path, b"x", 0o600).unwrap();
        assert_eq!(mode(&path), 0o600);
        let dir = tmp.path().join("group");
        create_dir(&dir, 0o775).unwrap();
        assert_eq!(mode(&dir), 0o775);
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

    /// The resolver op tells a missing file from a foreign one by this kind.
    /// A fresh Mac has no `/etc/resolver` folder either.
    #[test]
    fn read_of_a_missing_file_is_not_found() {
        let tmp = TempDir::new().unwrap();
        let in_folder = tmp.path().join("test");
        let no_folder = tmp.path().join("resolver").join("test");
        for path in [in_folder, no_folder] {
            let err = RealSystem.read(&path, 4096).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::NotFound, "{}", path.display());
            let err = FakeSystem::default().read(&path, 4096).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::NotFound);
        }
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

    #[cfg(unix)]
    #[test]
    fn run_starts_in_the_root_folder() {
        let output = RealSystem.run("/bin/pwd", &[]).unwrap();
        assert_eq!(output.stdout, b"/\n");
    }

    #[test]
    fn current_exe_is_absolute() {
        assert!(RealSystem.current_exe().unwrap().is_absolute());
    }

    #[test]
    fn fake_system_keeps_files_and_logs_commands() {
        let path = Path::new("/etc/resolver/test");
        let mut fake = FakeSystem::default();
        let output = |code| RunOutput {
            code: Some(code),
            ..RunOutput::default()
        };
        fake.script("/sbin/pfctl", &["-E"], output(0));
        fake.defaults.insert("/sbin/pfctl", output(1));
        fake.fail_removes.insert(path.to_path_buf());

        fake.write_atomic(path, b"port 18053", 0o644).unwrap();
        assert!(fake.exists(path));
        assert!(fake.read(path, 4).is_err());
        assert_eq!(fake.read(path, 10).unwrap(), b"port 18053");
        assert!(fake.remove(path).is_err());
        assert!(fake.exists(path));
        fake.fail_writes.insert(path.to_path_buf());
        assert!(fake.write_atomic(path, b"new", 0o644).is_err());
        assert_eq!(fake.read(path, 10).unwrap(), b"port 18053");

        assert_eq!(fake.run("/sbin/pfctl", &["-E"]).unwrap().code, Some(0));
        assert_eq!(fake.run("/sbin/pfctl", &["-d"]).unwrap().code, Some(1));
        assert!(fake.run("/usr/bin/true", &[]).is_err());
        let log = fake.log.borrow();
        assert_eq!(log[1], ("/sbin/pfctl".to_owned(), vec!["-d".to_owned()]));
        assert_eq!(log.len(), 3);
        assert_eq!(fake.current_exe().unwrap(), Path::new(fake::EXE));
    }
}
