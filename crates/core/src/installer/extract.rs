//! Unpacks a verified file (`tar.gz`, `zip`, or `raw`) into a temp folder by
//! the recipe's rules (`strip_components`, `into`). Refuses entries that would
//! write outside the folder, unsafe links, and archives over the size limits.
//!
//! Every entry goes through one function (`Sink::entry`), so the rules live in
//! one place. We never call `tar::Entry::unpack_in` or `ZipArchive::extract`.
//! On error nothing is cleaned up: the whole folder is temporary, and the
//! caller (M2-05) deletes it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

use crate::recipes::{ArchiveFormat, Artifact};

/// How one artifact is unpacked. Built from an [`Artifact`] with `From`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout<'a> {
    pub format: ArchiveFormat,
    /// Leading path components dropped from every entry (not used by `raw`).
    pub strip_components: u32,
    /// Folder under the destination, `/` between components, empty for none.
    pub into: &'a str,
    /// The saved name of a `raw` file. One component.
    pub file_name: Option<&'a str>,
}

impl<'a> From<&'a Artifact> for Layout<'a> {
    fn from(artifact: &'a Artifact) -> Self {
        Layout {
            format: artifact.format,
            strip_components: artifact.strip_components,
            into: &artifact.into,
            file_name: artifact.file_name.as_deref(),
        }
    }
}

/// Limits that stop an archive bomb, even if a bad file passed the checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes written in total, counted while copying (headers are not trusted).
    pub max_total_bytes: u64,
    /// Entries read, including folders, links, and skipped entries.
    pub max_entries: u64,
}

impl Default for Limits {
    /// 2 GiB and 200,000 entries. Node is about 190 MB and 6,000 files.
    fn default() -> Self {
        Limits {
            max_total_bytes: 2 * 1024 * 1024 * 1024,
            max_entries: 200_000,
        }
    }
}

/// What was written, for the log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractReport {
    /// Regular files and links written.
    pub files: u64,
    pub bytes: u64,
}

/// Why an archive was refused. The `String` is the entry name as written in
/// the archive (empty when the archive itself cannot be read).
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("archive entry {0:?} has an unsafe path")]
    UnsafePath(String),
    #[error("archive entry {0:?} is a link that is not allowed")]
    UnsafeLink(String),
    #[error("archive entry {0:?} has a type that is not supported")]
    UnsupportedEntry(String),
    #[error("archive entry {0:?} is listed twice")]
    Duplicate(String),
    #[error("archive is over the size or entry limit at {0:?}")]
    TooLarge(String),
    #[error("archive is damaged near entry {0:?}")]
    Corrupt(String, #[source] io::Error),
    #[error("cannot write {0:?}")]
    Io(PathBuf, #[source] io::Error),
}

impl ExtractError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            ExtractError::UnsafePath(..) => "extract_unsafe_path",
            ExtractError::UnsafeLink(..) => "extract_unsafe_link",
            ExtractError::UnsupportedEntry(..) => "extract_unsupported_entry",
            ExtractError::Duplicate(..) => "extract_duplicate",
            ExtractError::TooLarge(..) => "extract_too_large",
            ExtractError::Corrupt(..) => "extract_corrupt",
            ExtractError::Io(..) => "extract_io",
        }
    }
}

/// Unpacks `archive` into `dest_root`, which must exist. It may already hold
/// the files of an earlier artifact of the same version. Sync: the daemon
/// runs it in `spawn_blocking`.
///
/// # Errors
///
/// Any [`ExtractError`]. Files written before the error stay in `dest_root`.
pub fn extract(
    archive: &Path,
    layout: &Layout<'_>,
    dest_root: &Path,
    limits: &Limits,
) -> Result<ExtractReport, ExtractError> {
    let into = match layout.into {
        "" => Vec::new(),
        into => relative_parts(into).ok_or_else(|| ExtractError::UnsafePath(into.into()))?,
    };
    let mut sink = Sink {
        root: dest_root,
        into,
        strip: usize::try_from(layout.strip_components).unwrap_or(usize::MAX),
        limits,
        entries: 0,
        report: ExtractReport::default(),
    };
    let file = File::open(archive).map_err(io_err(archive))?;
    let mut reader = BufReader::new(file);

    match layout.format {
        ArchiveFormat::TarGz => {
            let mut tar = tar::Archive::new(GzDecoder::new(reader));
            for entry in tar.entries().map_err(corrupt(""))? {
                let mut entry = entry.map_err(corrupt(""))?;
                let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
                let header = entry.header();
                let kind = match header.entry_type() {
                    tar::EntryType::Regular | tar::EntryType::Continuous => Kind::File {
                        mode: header.mode().map_err(corrupt(&name))?,
                    },
                    tar::EntryType::Directory => Kind::Dir,
                    tar::EntryType::Symlink => Kind::Symlink {
                        target: entry.link_name_bytes().map(|t| t.into_owned()),
                    },
                    // A pax global header (from `git archive`) is metadata,
                    // never a path.
                    tar::EntryType::XGlobalHeader => Kind::Skip,
                    _ => Kind::Other,
                };
                sink.entry(&name, kind, &mut entry)?;
            }
        }
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipArchive::new(reader).map_err(corrupt(""))?;
            if zip.len() as u64 > limits.max_entries {
                return Err(ExtractError::TooLarge(String::new()));
            }
            for index in 0..zip.len() {
                let mut file = zip.by_index(index).map_err(corrupt(""))?;
                let name = file.name().map_err(corrupt(""))?.into_owned();
                // The type bits of a Unix mode: none, a regular file, or other.
                let file_type = file.unix_mode().unwrap_or(0) & 0o170_000;
                let kind = if file.is_symlink() {
                    Kind::Symlink { target: None }
                } else if file.is_dir() {
                    Kind::Dir
                } else if file_type == 0 || file_type == 0o100_000 {
                    Kind::File {
                        mode: file.unix_mode().unwrap_or(0o644),
                    }
                } else {
                    Kind::Other
                };
                sink.entry(&name, kind, &mut file)?;
            }
        }
        ArchiveFormat::Raw => {
            let name = layout.file_name.unwrap_or_default();
            if name.contains(['/', '\\']) {
                return Err(ExtractError::UnsafePath(name.into()));
            }
            sink.strip = 0;
            sink.entry(name, Kind::File { mode: 0o644 }, &mut reader)?;
        }
    }
    Ok(sink.report)
}

enum Kind {
    Dir,
    File {
        mode: u32,
    },
    /// `target` is `None` for a zip link: those are always refused.
    Symlink {
        target: Option<Vec<u8>>,
    },
    Skip,
    Other,
}

struct Sink<'a> {
    root: &'a Path,
    into: Vec<&'a str>,
    strip: usize,
    limits: &'a Limits,
    entries: u64,
    report: ExtractReport,
}

impl Sink<'_> {
    /// Checks one entry against every rule, then writes it.
    fn entry(&mut self, name: &str, kind: Kind, data: &mut dyn Read) -> Result<(), ExtractError> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(ExtractError::TooLarge(name.into()));
        }
        match kind {
            Kind::Skip => return Ok(()),
            Kind::Other => return Err(ExtractError::UnsupportedEntry(name.into())),
            _ => {}
        }
        let parts = relative_parts(name).ok_or_else(|| ExtractError::UnsafePath(name.into()))?;
        let Some(parts) = parts.get(self.strip..).filter(|p| !p.is_empty()) else {
            return Ok(()); // the stripped root folder
        };
        let rel: Vec<&str> = self.into.iter().chain(parts).copied().collect();
        let path = self.checked_path(name, &rel)?;

        if let Kind::Dir = kind {
            return fs::create_dir_all(&path).map_err(io_err(&path));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let unsafe_link = || ExtractError::UnsafeLink(name.into());
        match kind {
            Kind::File { mode } => self.write_file(name, &path, data, file_mode(mode))?,
            Kind::Symlink {
                target: Some(target),
            } => {
                let target = String::from_utf8(target).map_err(|_| unsafe_link())?;
                // Links are made on Unix only: Windows needs admin rights for
                // them, and no Windows archive we install has one.
                if !cfg!(unix) || !link_stays_inside(&rel, &target) {
                    return Err(unsafe_link());
                }
                symlink(&target, &path).map_err(|e| exists_or_io(name, &path, e))?;
            }
            _ => return Err(unsafe_link()),
        }
        self.report.files += 1;
        Ok(())
    }

    /// `root` joined with `rel`, one part at a time. Refuses the path if any
    /// part that exists is a link: we never write through one. We look with
    /// `symlink_metadata` (it does not follow links), and never `canonicalize`.
    fn checked_path(&self, entry: &str, rel: &[&str]) -> Result<PathBuf, ExtractError> {
        let mut path = self.root.to_path_buf();
        let mut exists = true;
        for part in rel {
            path.push(part);
            if !exists {
                continue;
            }
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(ExtractError::UnsafePath(entry.into()));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => exists = false,
                Err(e) => return Err(io_err(&path)(e)),
            }
        }
        Ok(path)
    }

    /// Copies `data` into a new file, counting bytes as they come, so a lying
    /// header cannot pass the limit.
    fn write_file(
        &mut self,
        entry: &str,
        path: &Path,
        data: &mut dyn Read,
        mode: u32,
    ) -> Result<(), ExtractError> {
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| exists_or_io(entry, path, e))?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match data.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(corrupt(entry)(e)),
            };
            self.report.bytes += n as u64;
            if self.report.bytes > self.limits.max_total_bytes {
                return Err(ExtractError::TooLarge(entry.into()));
            }
            out.write_all(&buf[..n]).map_err(io_err(path))?;
        }
        set_mode(&out, mode).map_err(io_err(path))
    }
}

/// Splits an entry name on `/` and `\` and checks every part. `None` if a part
/// is empty (absolute, UNC, or `a//b`), is `..` (or only dots and spaces,
/// which Windows may read as `..`), holds `:` (a drive or a stream), or the
/// name holds `\0`. `.` parts are dropped; a trailing separator is allowed.
fn relative_parts(name: &str) -> Option<Vec<&str>> {
    if name.contains('\0') {
        return None;
    }
    let mut parts: Vec<&str> = name.split(['/', '\\']).collect();
    while parts.len() > 1 && parts.last() == Some(&"") {
        parts.pop();
    }
    let bad = |p: &&str| {
        p.is_empty() || p.contains(':') || (*p != "." && p.chars().all(|c| c == '.' || c == ' '))
    };
    if parts.iter().any(bad) {
        return None;
    }
    Some(parts.into_iter().filter(|p| *p != ".").collect())
}

/// Whether a link at `link` (its parts under the root, name last) pointing to
/// `target` stays inside the root, judged on the text only. The target must
/// be relative, with `..` only at the start (`../../a/b`) and no more of them
/// than the link has folders above it. With no `..` after a name, a chain of
/// links cannot climb out either.
fn link_stays_inside(link: &[&str], target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') || target.contains(['\\', ':', '\0']) {
        return false;
    }
    let (mut ups, mut seen_name) = (0, false);
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." if seen_name => return false,
            ".." => ups += 1,
            _ => seen_name = true,
        }
    }
    ups < link.len()
}

/// Keeps only "executable or not": `0o755` or `0o644`. Drops setuid, setgid,
/// sticky, and write bits for others.
fn file_mode(archive_mode: u32) -> u32 {
    if archive_mode & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

// Mode bits and links exist on Unix only (task M2-03, rules 7 and 8). The
// helpers sit here, not in `platform`, because nothing else needs them.
#[cfg(unix)]
fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &str, path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(not(unix))]
fn symlink(_target: &str, _path: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> ExtractError + '_ {
    move |e| ExtractError::Io(path.to_path_buf(), e)
}

/// For errors while reading the archive. `E` is `io::Error` or `ZipError`.
fn corrupt<E: Into<io::Error>>(entry: &str) -> impl FnOnce(E) -> ExtractError + '_ {
    move |e| ExtractError::Corrupt(entry.to_string(), e.into())
}

fn exists_or_io(entry: &str, path: &Path, e: io::Error) -> ExtractError {
    if e.kind() == io::ErrorKind::AlreadyExists {
        ExtractError::Duplicate(entry.into())
    } else {
        io_err(path)(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::EntryType;
    use zip::write::SimpleFileOptions;

    const TAR: Layout<'static> = Layout {
        format: ArchiveFormat::TarGz,
        strip_components: 0,
        into: "",
        file_name: None,
    };
    const ZIP: Layout<'static> = Layout {
        format: ArchiveFormat::Zip,
        ..TAR
    };

    /// A tar.gz with headers written by hand, so names that `set_path`
    /// refuses (`../x`) can be tested. `data` is the target for links.
    fn targz(entries: &[(&str, EntryType, u32, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (name, kind, mode, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            if matches!(kind, EntryType::Symlink | EntryType::Link) {
                header.as_old_mut().linkname[..data.len()].copy_from_slice(data);
            }
            let body: &[u8] = if *kind == EntryType::Regular {
                data
            } else {
                &[]
            };
            header.set_entry_type(*kind);
            header.set_mode(*mode);
            header.set_size(body.len() as u64);
            header.set_cksum();
            builder.append(&header, body).expect("append");
        }
        builder.into_inner().expect("tar").finish().expect("gzip")
    }

    enum Z {
        File(&'static str, u32, &'static [u8]),
        Dir(&'static str),
        Link(&'static str, &'static str),
    }

    fn zip(entries: &[Z]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for entry in entries {
            match entry {
                Z::File(name, mode, data) => {
                    writer
                        .start_file(*name, options.unix_permissions(*mode))
                        .expect("file");
                    writer.write_all(data).expect("data");
                }
                Z::Dir(name) => writer.add_directory(*name, options).expect("dir"),
                Z::Link(name, target) => writer.add_symlink(*name, *target, options).expect("link"),
            }
        }
        writer.finish().expect("zip").into_inner()
    }

    /// Writes `bytes` to `tmp/archive` and unpacks it into `tmp/out`.
    fn unpack(
        tmp: &Path,
        bytes: &[u8],
        layout: &Layout,
        limits: &Limits,
    ) -> Result<ExtractReport, ExtractError> {
        let archive = tmp.join("archive");
        fs::write(&archive, bytes).expect("write archive");
        fs::create_dir_all(tmp.join("out")).expect("out");
        extract(&archive, layout, &tmp.join("out"), limits)
    }

    /// The error code for `bytes`, after checking nothing landed next to `out`.
    fn refused(bytes: &[u8], layout: &Layout, limits: &Limits) -> &'static str {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = unpack(tmp.path(), bytes, layout, limits).expect_err("must be refused");
        assert_eq!(fs::read_dir(tmp.path()).expect("list").count(), 2, "{err}");
        err.code()
    }

    fn code(bytes: &[u8], layout: &Layout) -> &'static str {
        refused(bytes, layout, &Limits::default())
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
    }

    #[test]
    fn entry_names_are_split_and_checked() {
        assert_eq!(relative_parts("a/b\\c/"), Some(vec!["a", "b", "c"]));
        assert_eq!(relative_parts("./node/bin"), Some(vec!["node", "bin"]));
        assert_eq!(relative_parts("./"), Some(vec![]));
        let bad = [
            "",
            "/",
            "/abs",
            "../evil",
            "a/../../evil",
            "..\\evil",
            "C:\\evil",
            "C:evil",
        ];
        let more = [
            "\\\\?\\C:\\x",
            "\\\\host\\share",
            "a//b",
            "a/\0b",
            "...",
            ".. ",
            "a:b",
        ];
        for name in bad.into_iter().chain(more) {
            assert_eq!(relative_parts(name), None, "{name:?}");
        }
    }

    #[test]
    fn link_targets_must_stay_inside() {
        assert!(link_stays_inside(
            &["bin", "npm"],
            "../lib/node_modules/npm/bin/npm-cli.js"
        ));
        assert!(link_stays_inside(&["a", "b", "l"], "../../x"));
        assert!(link_stays_inside(&["l"], "./x/y"));
        for (link, target) in [
            (&["l"][..], "../x"),
            (&["a", "l"], "../../x"),
            (&["a", "l"], "x/../../y"),
            (&["a", "b", "l"], "../x/../y"),
            (&["a", "l"], "/etc"),
            (&["a", "l"], "C:/x"),
            (&["a", "l"], "..\\x"),
            (&["a", "l"], ""),
        ] {
            assert!(!link_stays_inside(link, target), "{link:?} -> {target:?}");
        }
    }

    #[test]
    fn only_755_or_644_are_kept() {
        for (from, to) in [
            (0o4755, 0o755),
            (0o2711, 0o755),
            (0o777, 0o755),
            (0o666, 0o644),
            (0o1644, 0o644),
            (0, 0o644),
        ] {
            assert_eq!(file_mode(from), to, "{from:o}");
        }
    }

    #[test]
    fn node_like_tar_gz_with_strip_components() {
        let bytes = targz(&[
            ("node-v22/", EntryType::Directory, 0o755, b""),
            ("node-v22/bin/node", EntryType::Regular, 0o755, b"node"),
            (
                "node-v22/lib/node_modules/npm/bin/npm-cli.js",
                EntryType::Regular,
                0o644,
                b"npm",
            ),
            (
                "node-v22/bin/npm",
                EntryType::Symlink,
                0o777,
                b"../lib/node_modules/npm/bin/npm-cli.js",
            ),
        ]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = Layout {
            strip_components: 1,
            ..TAR
        };
        let result = unpack(tmp.path(), &bytes, &layout, &Limits::default());
        let bin = tmp.path().join("out").join("bin");
        assert_eq!(fs::read(bin.join("node")).expect("node"), b"node");
        if cfg!(unix) {
            assert_eq!(
                result.expect("unpacked"),
                ExtractReport { files: 3, bytes: 7 }
            );
            assert_eq!(fs::read(bin.join("npm")).expect("link works"), b"npm");
        } else {
            assert_eq!(
                result.expect_err("no links on Windows").code(),
                "extract_unsafe_link"
            );
        }
        #[cfg(unix)]
        assert_eq!(mode(&bin.join("node")), 0o755);
    }

    #[test]
    fn static_php_into_bin_then_a_second_archive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = Layout { into: "bin", ..TAR };
        let cli = targz(&[("php", EntryType::Regular, 0o755, b"cli")]);
        let fpm = targz(&[("php-fpm", EntryType::Regular, 0o755, b"fpm")]);
        unpack(tmp.path(), &cli, &layout, &Limits::default()).expect("cli");
        unpack(tmp.path(), &fpm, &layout, &Limits::default()).expect("fpm");
        let bin = tmp.path().join("out").join("bin");
        assert_eq!(fs::read(bin.join("php")).expect("php"), b"cli");
        assert_eq!(fs::read(bin.join("php-fpm")).expect("php-fpm"), b"fpm");
    }

    #[test]
    fn windows_php_like_zip() {
        let bytes = zip(&[
            Z::File("php.exe", 0o644, b"exe"),
            Z::Dir("ext/"),
            Z::File("ext/php_curl.dll", 0o644, b"curl"),
            Z::File("dev\\php8.lib", 0o644, b"lib"),
        ]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = unpack(tmp.path(), &bytes, &ZIP, &Limits::default()).expect("unpacked");
        assert_eq!(
            report,
            ExtractReport {
                files: 3,
                bytes: 10
            }
        );
        let out = tmp.path().join("out");
        assert_eq!(fs::read(out.join("php.exe")).expect("exe"), b"exe");
        assert_eq!(
            fs::read(out.join("ext").join("php_curl.dll")).expect("dll"),
            b"curl"
        );
        assert_eq!(
            fs::read(out.join("dev").join("php8.lib")).expect("lib"),
            b"lib"
        );
    }

    #[test]
    fn raw_file_is_copied_under_its_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = Layout {
            format: ArchiveFormat::Raw,
            file_name: Some("composer.phar"),
            ..TAR
        };
        let report = unpack(tmp.path(), b"<?php", &layout, &Limits::default()).expect("copied");
        assert_eq!(report, ExtractReport { files: 1, bytes: 5 });
        let phar = tmp.path().join("out").join("composer.phar");
        assert_eq!(fs::read(&phar).expect("phar"), b"<?php");
        #[cfg(unix)]
        assert_eq!(mode(&phar), 0o644);
        let bad = Layout {
            file_name: Some("../composer.phar"),
            ..layout
        };
        assert_eq!(code(b"<?php", &bad), "extract_unsafe_path");
    }

    #[test]
    fn tar_paths_that_escape_are_refused() {
        for name in ["../evil", "/abs", "a/../../evil", "..\\evil", "C:\\evil"] {
            let bytes = targz(&[(name, EntryType::Regular, 0o644, b"x")]);
            assert_eq!(code(&bytes, &TAR), "extract_unsafe_path", "{name:?}");
        }
        let into = Layout {
            into: "../up",
            ..TAR
        };
        assert_eq!(code(&targz(&[]), &into), "extract_unsafe_path");
    }

    #[test]
    fn zip_paths_that_escape_are_refused() {
        for name in [
            "../evil",
            "/abs",
            "C:\\evil",
            "..\\evil",
            "\\\\?\\C:\\evil",
            "a\0b",
        ] {
            let bytes = zip(&[Z::File(name, 0o644, b"x")]);
            assert_eq!(code(&bytes, &ZIP), "extract_unsafe_path", "{name:?}");
        }
    }

    #[test]
    fn tar_links_that_leave_the_root_are_refused() {
        for (name, target) in [("etc", "/etc"), ("a/l", "../../x"), ("a/l", "x/../../y")] {
            let bytes = targz(&[(name, EntryType::Symlink, 0o777, target.as_bytes())]);
            assert_eq!(
                code(&bytes, &TAR),
                "extract_unsafe_link",
                "{name} -> {target}"
            );
        }
    }

    // Unix only: Windows refuses every link before this rule is reached.
    #[cfg(unix)]
    #[test]
    fn nothing_is_written_through_a_link() {
        let link: (&str, EntryType, u32, &[u8]) = ("lib2", EntryType::Symlink, 0o777, b"lib");
        let dir = ("lib/", EntryType::Directory, 0o755, &b""[..]);
        for name in ["lib2/x", "lib2"] {
            let bytes = targz(&[dir, link, (name, EntryType::Regular, 0o644, b"x")]);
            assert_eq!(code(&bytes, &TAR), "extract_unsafe_path", "{name}");
        }
    }

    #[test]
    fn hard_links_devices_and_fifos_are_refused() {
        for kind in [
            EntryType::Link,
            EntryType::Char,
            EntryType::Block,
            EntryType::Fifo,
        ] {
            let bytes = targz(&[("x", kind, 0o644, b"target")]);
            assert_eq!(code(&bytes, &TAR), "extract_unsupported_entry", "{kind:?}");
        }
    }

    #[test]
    fn zip_links_are_refused() {
        assert_eq!(
            code(&zip(&[Z::Link("l", "x")]), &ZIP),
            "extract_unsafe_link"
        );
    }

    // Unix only: Windows files have no mode bits.
    #[cfg(unix)]
    #[test]
    fn setuid_and_setgid_are_dropped() {
        let bytes = targz(&[
            ("suid", EntryType::Regular, 0o4755, b"x"),
            ("sgid", EntryType::Regular, 0o2775, b"x"),
            ("open", EntryType::Regular, 0o666, b"x"),
        ]);
        let tmp = tempfile::tempdir().expect("tempdir");
        unpack(tmp.path(), &bytes, &TAR, &Limits::default()).expect("unpacked");
        let out = tmp.path().join("out");
        assert_eq!(mode(&out.join("suid")), 0o755);
        assert_eq!(mode(&out.join("sgid")), 0o755);
        assert_eq!(mode(&out.join("open")), 0o644);
    }

    #[test]
    fn a_second_entry_with_the_same_path_is_refused() {
        let file = ("a", EntryType::Regular, 0o644, &b"x"[..]);
        assert_eq!(code(&targz(&[file, file]), &TAR), "extract_duplicate");
    }

    #[test]
    fn bytes_over_the_limit_stop_the_copy() {
        let limits = Limits {
            max_total_bytes: 1024,
            ..Limits::default()
        };
        let bytes = targz(&[("big", EntryType::Regular, 0o644, &[7u8; 2048])]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = unpack(tmp.path(), &bytes, &TAR, &limits).expect_err("too large");
        assert_eq!(err.code(), "extract_too_large");
        let written = fs::metadata(tmp.path().join("out").join("big"))
            .expect("big")
            .len();
        assert!(written < 2048, "{written}");
    }

    #[test]
    fn entries_over_the_limit_are_refused() {
        let limits = Limits {
            max_entries: 2,
            ..Limits::default()
        };
        let file = |name| (name, EntryType::Regular, 0o644, &b"x"[..]);
        let tar = targz(&[file("a"), file("b"), file("c")]);
        assert_eq!(refused(&tar, &TAR, &limits), "extract_too_large");
        let zip = zip(&[Z::Dir("a/"), Z::Dir("b/"), Z::Dir("c/")]);
        assert_eq!(refused(&zip, &ZIP, &limits), "extract_too_large");
    }

    #[test]
    fn damaged_archives_are_refused() {
        let bytes = targz(&[("a", EntryType::Regular, 0o644, &[1u8; 4096])]);
        assert_eq!(code(&bytes[..bytes.len() / 2], &TAR), "extract_corrupt");
        assert_eq!(code(b"this is not a zip file", &ZIP), "extract_corrupt");
    }
}
