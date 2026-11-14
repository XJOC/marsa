//! A file that is Marsa's only if its first line is exactly [`MARKER`].
//!
//! The Linux ops write small config files in folders that other tools use
//! too (`/etc/sysctl.d`, `/etc/systemd/resolved.conf.d`). Before the helper
//! changes or deletes such a file, it reads it here: a file without the
//! marker belongs to someone else and is never written over or deleted.
//! Marsa never writes a link, so a link at the path is someone else's too,
//! whatever its target holds.
//!
//! `resolver` (macOS, task M4-07) has its own copy of this check.

use std::io;
use std::path::Path;

use crate::system::System;

/// The first line of every file the Linux ops write.
pub const MARKER: &str = "# Managed by Marsa. Do not edit.";
/// The largest file read. Marsa's files are far smaller, so a larger one
/// counts as foreign.
const MAX_BYTES: u64 = 4 * 1024;

/// What is at a path now.
#[derive(Debug, PartialEq, Eq)]
pub enum Current {
    Missing,
    /// Marsa's file, with its content.
    Ours(Vec<u8>),
    /// Another tool's file, a file over [`MAX_BYTES`], a folder, or a
    /// symlink (to anything, or to nothing).
    Foreign,
    /// Reading failed for another reason. Left as it is too, but an op
    /// reports it with its own code, so the message does not blame another
    /// tool.
    Unreadable(io::ErrorKind),
}

/// Reads `path` and says whose file it is.
pub fn current(system: &impl System, path: &Path) -> Current {
    // First: `read` follows a link, so a link to a file with the marker
    // would read as Marsa's, and the op would replace or delete the link.
    if system.is_link(path) {
        return Current::Foreign;
    }
    match system.read(path, MAX_BYTES) {
        Ok(bytes) if is_ours(&bytes) => Current::Ours(bytes),
        // `read` follows symlinks, so a dangling link also says `NotFound`.
        // `exists` does not follow them: a link there is someone else's.
        Err(err) if err.kind() == io::ErrorKind::NotFound && !system.exists(path) => {
            Current::Missing
        }
        // Fail safe: anything the helper cannot read in full is never
        // changed. `InvalidData` is a file over the limit and `InvalidInput`
        // is not a regular file (`System::read`); a dangling link is
        // `NotFound` with something at the path.
        Ok(_) => Current::Foreign,
        Err(err) => match err.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => {
                Current::Foreign
            }
            kind => Current::Unreadable(kind),
        },
    }
}

/// The first line is exactly the marker. Nothing is trimmed: a near match
/// (a `\r`, a space) is not a file Marsa wrote, so it counts as foreign.
fn is_ours(bytes: &[u8]) -> bool {
    bytes.split(|&byte| byte == b'\n').next() == Some(MARKER.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::system::FakeSystem;

    const PATH: &str = "/etc/sysctl.d/60-marsa.conf";

    fn fake_with(bytes: &[u8]) -> FakeSystem {
        let fake = FakeSystem::default();
        let file = (bytes.to_vec(), 0o644);
        fake.files.borrow_mut().insert(PathBuf::from(PATH), file);
        fake
    }

    fn at(fake: &FakeSystem) -> Current {
        current(fake, Path::new(PATH))
    }

    #[test]
    fn no_file_is_missing() {
        assert_eq!(at(&FakeSystem::default()), Current::Missing);
    }

    #[test]
    fn only_the_exact_marker_line_makes_a_file_ours() {
        let ours = format!("{MARKER}\nkey=1\n").into_bytes();
        assert_eq!(at(&fake_with(&ours)), Current::Ours(ours));
        // The marker alone, with no newline, is still the first line.
        let bare = MARKER.as_bytes().to_vec();
        assert_eq!(at(&fake_with(&bare)), Current::Ours(bare));
        let near = [
            format!("{MARKER}\r\nkey=1\n"),
            format!("{MARKER} \n"),
            format!(" {MARKER}\n"),
            format!("\n{MARKER}\n"),
            MARKER.to_lowercase(),
            "key=1\n".to_owned(),
            String::new(),
        ];
        for text in near {
            assert_eq!(
                at(&fake_with(text.as_bytes())),
                Current::Foreign,
                "{text:?}"
            );
        }
    }

    /// The marker is there, but the size alone makes it foreign.
    #[test]
    fn a_file_over_4_kib_is_foreign() {
        let mut bytes = format!("{MARKER}\n").into_bytes();
        bytes.resize(4096, b'#');
        assert!(matches!(at(&fake_with(&bytes)), Current::Ours(_)));
        bytes.push(b'#');
        assert_eq!(at(&fake_with(&bytes)), Current::Foreign);
    }

    /// `read` follows the link and says `NotFound`; the link is still
    /// someone else's.
    #[test]
    fn a_dangling_symlink_is_foreign() {
        let mut fake = FakeSystem::default();
        fake.dangling.insert(PathBuf::from(PATH));
        assert_eq!(at(&fake), Current::Foreign);
    }

    /// A link to a file that starts with the marker reads as Marsa's, but
    /// Marsa never writes a link.
    #[test]
    fn a_symlink_to_a_marked_file_is_foreign() {
        let mut fake = fake_with(format!("{MARKER}\nkey=1\n").as_bytes());
        fake.links.insert(PathBuf::from(PATH));
        assert_eq!(at(&fake), Current::Foreign);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_unreadable() {
        let mut fake = fake_with(MARKER.as_bytes());
        fake.fail_reads.insert(PathBuf::from(PATH));
        assert_eq!(
            at(&fake),
            Current::Unreadable(io::ErrorKind::PermissionDenied)
        );
    }
}
