//! Reads a Laravel site's own log: the newest `laravel*.log` in the site's
//! `storage/logs`, and nothing outside that folder.
//!
//! A project is the user's, and may come from anywhere: a cloned repository
//! can hold `storage/logs` as a link to `~/.ssh`. So before each read:
//!
//! - `storage/logs` must resolve to a folder inside the resolved site root;
//! - the file must be a regular file, not a link, when looked at by path,
//!   and the same file once open (device and inode on Unix; on Windows the
//!   open does not follow a link, `platform::files::open_no_follow`), with
//!   one name only on Unix (no hard link to a file elsewhere);
//! - the folder is checked again after the open, so a swap of the folder in
//!   between is seen too.
//!
//! What is left needs another program of the same user that swaps paths at
//! the right moment, and that program could read the file itself: a folder
//! swapped out and back between the two checks, or a file swapped for a FIFO
//! between the check and the open (the open then waits; callers run the
//! reader on a blocking thread). `O_NOFOLLOW | O_NONBLOCK` would close both,
//! and needs the `libc` crate.
//!
//! The file is opened anew for each look and closed after it, so the reader
//! never holds a file Laravel wants to rotate.

use std::fs::{self, File, Metadata};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use marsa_core::codes;
use marsa_core::fs::{file_id, link_count};
use marsa_core::platform::files::open_no_follow;
use marsa_core::sitelog::{self, LineSplitter, MAX_TAIL_BYTES, MAX_TAIL_LINES};
use marsa_core::types::{SiteLog, SiteLogLine};

/// The size of one read. Lines are cut at `sitelog::MAX_LINE_BYTES`, so a
/// read never holds more than this and one line.
const CHUNK: usize = 64 * 1024;

/// Why the log cannot be read.
#[derive(Debug)]
pub(crate) enum LogError {
    /// `storage/logs` leads out of the site, or the file is a link or not a
    /// regular file. The text says which, for the daemon's log; it holds no
    /// path.
    Unsafe(&'static str),
    /// The folder or the file cannot be read.
    Io(io::Error),
}

impl LogError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Unsafe(_) => codes::SITE_LOG_UNSAFE,
            Self::Io(_) => codes::INTERNAL_ERROR,
        }
    }
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsafe(why) => write!(f, "the site's log is not safe to read: {why}"),
            Self::Io(err) => write!(f, "cannot read the site's log: {err}"),
        }
    }
}

/// A site's `storage/logs`.
#[derive(Debug)]
struct LogDir {
    root: PathBuf,
    dir: PathBuf,
}

impl LogDir {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            dir: root.join("storage").join("logs"),
        }
    }

    /// `Ok(false)`: no folder yet, which is not an error. `canonicalize` is
    /// used to compare only; its path never leaves this function.
    fn check(&self) -> Result<bool, LogError> {
        let meta = match fs::symlink_metadata(&self.dir) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) if err.kind() == io::ErrorKind::NotADirectory => {
                return Err(LogError::Unsafe("storage is not a folder"));
            }
            other => other.map_err(LogError::Io)?,
        };
        let is_link = meta.file_type().is_symlink();
        if !meta.is_dir() && !is_link {
            return Err(LogError::Unsafe("storage/logs is not a folder"));
        }
        let root = fs::canonicalize(&self.root).map_err(LogError::Io)?;
        let dir = match fs::canonicalize(&self.dir) {
            Ok(dir) => dir,
            Err(err) if err.kind() == io::ErrorKind::NotFound && !is_link => return Ok(false),
            // A link to nothing, a loop of links, or a part that is a file.
            Err(_) => return Err(LogError::Unsafe("storage/logs cannot be resolved")),
        };
        if !dir.starts_with(&root) {
            return Err(LogError::Unsafe("storage/logs leads out of the site"));
        }
        if !fs::metadata(&dir).map_err(LogError::Io)?.is_dir() {
            return Err(LogError::Unsafe("storage/logs is not a folder"));
        }
        Ok(true)
    }

    /// The name of the newest Laravel log in the folder (`sitelog::newest`),
    /// or `None`. An entry that cannot be read is left out.
    fn newest(&self) -> Result<Option<String>, LogError> {
        let entries = match fs::read_dir(&self.dir) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other.map_err(LogError::Io)?,
        };
        let files: Vec<(String, std::time::SystemTime)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                // The name first: a stat only for Laravel's own files.
                if !sitelog::is_log_name(&name) {
                    return None;
                }
                // `DirEntry::metadata` does not follow a link.
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((name, modified))
            })
            .collect();
        let newest = sitelog::newest(files.iter().map(|(name, at)| (name.as_str(), *at)));
        Ok(newest.map(str::to_owned))
    }

    /// Opens the log `name` with the checks of the module. `None`: it is gone.
    fn open(&self, name: &str) -> Result<Option<(File, Metadata)>, LogError> {
        let path = self.dir.join(name);
        let before = match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other.map_err(LogError::Io)?,
        };
        // A link is refused, and so is a FIFO, whose open would wait for a
        // writer without end.
        if !before.is_file() {
            return Err(LogError::Unsafe(
                "the log file is a link or not a regular file",
            ));
        }
        let file = match open_no_follow(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other.map_err(LogError::Io)?,
        };
        let opened = file.metadata().map_err(LogError::Io)?;
        if !opened.is_file() || file_id(&before) != file_id(&opened) {
            return Err(LogError::Unsafe("the log file changed while it was opened"));
        }
        if link_count(&opened).is_some_and(|names| names > 1) {
            return Err(LogError::Unsafe(
                "the log file has another name (a hard link)",
            ));
        }
        if !self.check()? {
            return Ok(None);
        }
        Ok(Some((file, opened)))
    }
}

/// Where a reader is in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    name: String,
    /// Device and inode on Unix; `None` on Windows.
    id: Option<(u64, u64)>,
    /// Where the next read starts.
    pos: u64,
}

/// One reader of a site's log: the end of the newest file, then each line
/// written after it. One per request or stream, so nothing runs without a
/// reader.
#[derive(Debug)]
pub(crate) struct Tail {
    dir: LogDir,
    /// The file read now.
    current: Option<Place>,
    splitter: LineSplitter,
    /// The `seq` of the next line.
    seq: u64,
}

impl Tail {
    /// Reads the last [`MAX_TAIL_BYTES`] (and at most [`MAX_TAIL_LINES`]
    /// lines) of the newest log under `root`, and gives a reader that
    /// follows it from there. A last line without its line end stays in the
    /// reader: [`Tail::finish`] gives it.
    ///
    /// # Errors
    ///
    /// [`LogError::Unsafe`], or a read that failed.
    pub(crate) fn start(root: &Path) -> Result<(SiteLog, Self), LogError> {
        let mut tail = Self {
            dir: LogDir::new(root),
            current: None,
            splitter: LineSplitter::new(false),
            seq: 0,
        };
        let mut log = SiteLog::default();
        let newest = if tail.dir.check()? {
            tail.dir.newest()?
        } else {
            None
        };
        let opened = match newest {
            Some(name) => tail.dir.open(&name)?.map(|opened| (name, opened)),
            None => None,
        };
        let Some((name, (mut file, meta))) = opened else {
            return Ok((log, tail));
        };
        let len = meta.len();
        let at = tail.near_end(len, MAX_TAIL_BYTES);
        let mut texts = Vec::new();
        let read = read_into(
            &mut file,
            at,
            len - at,
            &mut tail.splitter,
            &mut texts,
            Lines::KeepLast(MAX_TAIL_LINES),
        );
        if let Some(err) = read.error {
            return Err(LogError::Io(err));
        }
        tail.current = Some(Place {
            name: name.clone(),
            id: file_id(&meta),
            pos: at + read.bytes,
        });
        log.file = Some(name);
        log.cut_at_start = at > 0 || read.dropped;
        log.lines = tail.number(texts);
        Ok((log, tail))
    }

    /// The last line of the file when it has no line end, for an answer that
    /// does not follow.
    pub(crate) fn finish(&mut self) -> Option<SiteLogLine> {
        let text = self.splitter.finish()?;
        self.number(vec![text]).pop()
    }

    /// Where to start reading the last `max` bytes of a file of `len` bytes,
    /// with a fresh splitter for it. The read starts one byte early, so a
    /// start just after a line end is seen as such: that byte is then the
    /// whole first piece, which is dropped.
    fn near_end(&mut self, len: u64, max: u64) -> u64 {
        let from = len.saturating_sub(max);
        self.splitter = LineSplitter::new(from > 0);
        from.saturating_sub(1)
    }

    fn number(&mut self, texts: Vec<String>) -> Vec<SiteLogLine> {
        texts
            .into_iter()
            .map(|text| {
                let line = SiteLogLine {
                    seq: self.seq,
                    text,
                };
                self.seq += 1;
                line
            })
            .collect()
    }
}

/// How a read bounds the number of lines it gives.
#[derive(Debug, Clone, Copy)]
enum Lines {
    /// Read all, keep the last `n`.
    KeepLast(usize),
}

/// What [`read_into`] did.
#[derive(Debug)]
struct Read {
    /// The bytes given to the splitter.
    bytes: u64,
    /// Lines were dropped to keep the last ones.
    dropped: bool,
    /// The read stopped on this error.
    error: Option<io::Error>,
}

/// Reads at most `len` bytes of `file` from `from` into `splitter`: fewer if
/// the file became shorter, or `lines` says stop.
fn read_into(
    file: &mut File,
    from: u64,
    len: u64,
    splitter: &mut LineSplitter,
    out: &mut Vec<String>,
    lines: Lines,
) -> Read {
    let mut read = Read {
        bytes: 0,
        dropped: false,
        error: None,
    };
    if let Err(err) = file.seek(SeekFrom::Start(from)) {
        read.error = Some(err);
        return read;
    }
    let mut limited = file.take(len);
    let mut buf = vec![0; CHUNK];
    loop {
        let n = match limited.read(&mut buf) {
            Ok(0) => return read,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                read.error = Some(err);
                return read;
            }
        };
        splitter.push(&buf[..n], out);
        read.bytes += n as u64;
        match lines {
            Lines::KeepLast(keep) if out.len() > keep => {
                out.drain(..out.len() - keep);
                read.dropped = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// A site root with `storage/logs`, and its temp folder kept alive.
    fn site() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("shop");
        fs::create_dir_all(root.join("storage/logs")).unwrap();
        (temp, root)
    }

    fn logs(root: &Path) -> PathBuf {
        root.join("storage/logs")
    }

    fn texts(lines: &[SiteLogLine]) -> Vec<&str> {
        lines.iter().map(|line| line.text.as_str()).collect()
    }

    fn touch(path: &Path, secs: u64) {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    fn unsafe_start(root: &Path) -> &'static str {
        match Tail::start(root) {
            Err(err @ LogError::Unsafe(_)) => err.code(),
            other => panic!("expected {}, got {other:?}", codes::SITE_LOG_UNSAFE),
        }
    }

    #[test]
    fn no_folder_or_no_file_is_an_empty_log() {
        let temp = tempfile::tempdir().unwrap();
        let (log, _) = Tail::start(temp.path()).unwrap();
        assert_eq!(log, SiteLog::default());
        let (_temp, root) = site();
        fs::write(logs(&root).join("worker.log"), "not laravel\n").unwrap();
        let (log, _) = Tail::start(&root).unwrap();
        assert_eq!(log, SiteLog::default());
    }

    #[test]
    fn a_small_file_is_read_whole_and_numbered() {
        let (_temp, root) = site();
        fs::write(logs(&root).join("laravel.log"), "one\r\ntwo\n\nlast").unwrap();
        let (log, mut tail) = Tail::start(&root).unwrap();
        assert_eq!(log.file.as_deref(), Some("laravel.log"));
        assert!(!log.cut_at_start);
        assert_eq!(texts(&log.lines), ["one", "two", ""]);
        assert_eq!(
            log.lines.iter().map(|l| l.seq).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        let last = tail.finish().unwrap();
        assert_eq!((last.seq, last.text.as_str()), (3, "last"));
        assert_eq!(tail.finish(), None);
    }

    #[test]
    fn an_empty_file_has_a_name_and_no_lines() {
        let (_temp, root) = site();
        fs::write(logs(&root).join("laravel.log"), "").unwrap();
        let (log, mut tail) = Tail::start(&root).unwrap();
        assert_eq!(log.file.as_deref(), Some("laravel.log"));
        assert!(log.lines.is_empty());
        assert_eq!(tail.finish(), None);
    }

    #[test]
    fn a_big_file_gives_its_last_mebibyte_from_a_whole_line() {
        let (_temp, root) = site();
        // 2 MiB of 200-byte lines (fewer than `MAX_TAIL_LINES` in 1 MiB):
        // the cut falls inside a line.
        let line = format!("{}\n", "x".repeat(199));
        let mut text = String::new();
        for n in 0..(2 * 1024 * 1024 / 200) {
            text.push_str(&format!("{n:08}"));
            text.push_str(&line[8..]);
        }
        fs::write(logs(&root).join("laravel.log"), &text).unwrap();
        let (log, _) = Tail::start(&root).unwrap();
        assert!(log.cut_at_start);
        assert!(log.lines.iter().all(|line| line.text.len() == 199));
        let kept: usize = log.lines.iter().map(|line| line.text.len() + 1).sum();
        assert!(kept <= MAX_TAIL_BYTES as usize && kept > MAX_TAIL_BYTES as usize - 200);
        assert_eq!(log.lines.last().unwrap().text, text.lines().last().unwrap());
    }

    #[test]
    fn a_cut_just_after_a_line_end_keeps_the_next_line() {
        let (_temp, root) = site();
        let first = "a".repeat(99);
        let rest = "b".repeat(MAX_TAIL_BYTES as usize - 1);
        fs::write(
            logs(&root).join("laravel.log"),
            format!("{first}\n{rest}\n"),
        )
        .unwrap();
        let (log, _) = Tail::start(&root).unwrap();
        assert!(log.cut_at_start);
        assert_eq!(log.lines.len(), 1);
        assert!(log.lines[0].text.starts_with('b'));
    }

    #[test]
    fn a_long_line_is_cut() {
        let (_temp, root) = site();
        let text = format!("{}\nshort\n", "y".repeat(100 * 1024));
        fs::write(logs(&root).join("laravel.log"), text).unwrap();
        let (log, _) = Tail::start(&root).unwrap();
        assert_eq!(log.lines.len(), 2);
        assert!(log.lines[0].text.ends_with(sitelog::CUT_MARK));
        assert_eq!(log.lines[1].text, "short");
    }

    #[test]
    fn the_newest_daily_file_is_read() {
        let (_temp, root) = site();
        let dir = logs(&root);
        fs::write(dir.join("laravel-2026-11-23.log"), "old\n").unwrap();
        fs::write(dir.join("laravel-2026-11-24.log"), "new\n").unwrap();
        fs::write(dir.join("laravel.log.1"), "rotated\n").unwrap();
        touch(&dir.join("laravel-2026-11-23.log"), 1);
        touch(&dir.join("laravel-2026-11-24.log"), 2);
        touch(&dir.join("laravel.log.1"), 3);
        let (log, _) = Tail::start(&root).unwrap();
        assert_eq!(log.file.as_deref(), Some("laravel-2026-11-24.log"));
        assert_eq!(texts(&log.lines), ["new"]);
    }

    #[test]
    fn storage_logs_as_a_file_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("storage")).unwrap();
        fs::write(temp.path().join("storage/logs"), "").unwrap();
        assert_eq!(unsafe_start(temp.path()), codes::SITE_LOG_UNSAFE);
    }

    #[test]
    fn storage_as_a_file_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("storage"), "").unwrap();
        assert_eq!(unsafe_start(temp.path()), codes::SITE_LOG_UNSAFE);
    }

    #[test]
    fn the_first_read_keeps_the_last_lines_only() {
        let (_temp, root) = site();
        let mut text = "\n".repeat(3 * MAX_TAIL_LINES);
        text.push_str("last\n");
        fs::write(logs(&root).join("laravel.log"), text).unwrap();
        let (log, _) = Tail::start(&root).unwrap();
        assert!(log.cut_at_start);
        assert_eq!(log.lines.len(), MAX_TAIL_LINES);
        assert_eq!(log.lines.last().unwrap().text, "last");
    }

    // Links are made with `std::os::unix::fs::symlink`; Windows has its own
    // tests below.
    #[cfg(unix)]
    mod links {
        use super::*;
        use std::os::unix::fs::symlink;

        #[test]
        fn storage_logs_linked_out_of_the_site_is_refused() {
            let (temp, root) = site();
            let outside = temp.path().join("elsewhere");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("laravel.log"), "secret\n").unwrap();
            fs::remove_dir(logs(&root)).unwrap();
            symlink(&outside, logs(&root)).unwrap();
            assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
        }

        #[test]
        fn storage_linked_out_of_the_site_is_refused() {
            let (temp, root) = site();
            let outside = temp.path().join("shared-storage");
            fs::create_dir_all(outside.join("logs")).unwrap();
            fs::remove_dir_all(root.join("storage")).unwrap();
            symlink(&outside, root.join("storage")).unwrap();
            assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
        }

        #[test]
        fn a_link_to_nothing_is_refused() {
            let (temp, root) = site();
            fs::remove_dir(logs(&root)).unwrap();
            symlink(temp.path().join("missing"), logs(&root)).unwrap();
            assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
        }

        #[test]
        fn storage_logs_linked_inside_the_site_is_read() {
            let (_temp, root) = site();
            fs::create_dir(root.join("other-logs")).unwrap();
            fs::write(root.join("other-logs/laravel.log"), "inside\n").unwrap();
            fs::remove_dir(logs(&root)).unwrap();
            symlink(root.join("other-logs"), logs(&root)).unwrap();
            let (log, _) = Tail::start(&root).unwrap();
            assert_eq!(texts(&log.lines), ["inside"]);
        }

        #[test]
        fn a_log_file_that_is_a_link_is_refused_even_to_a_file_inside() {
            let (temp, root) = site();
            let secret = temp.path().join("hosts");
            fs::write(&secret, "127.0.0.1 secret\n").unwrap();
            symlink(&secret, logs(&root).join("laravel.log")).unwrap();
            assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);

            let (_temp, root) = site();
            fs::write(root.join("inside.txt"), "x\n").unwrap();
            symlink(root.join("inside.txt"), logs(&root).join("laravel.log")).unwrap();
            assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
        }

        #[test]
        fn a_hard_link_is_refused() {
            let (temp, root) = site();
            let secret = temp.path().join("secret");
            fs::write(&secret, "secret\n").unwrap();
            fs::hard_link(&secret, logs(&root).join("laravel.log")).unwrap();
            assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
        }

        #[test]
        fn a_file_that_is_not_regular_is_refused_without_an_open() {
            let (_temp, root) = site();
            // A socket: not a regular file, made with std only. A FIFO takes
            // the same branch.
            let _socket =
                std::os::unix::net::UnixListener::bind(logs(&root).join("laravel.log")).unwrap();
            assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
        }

        #[test]
        fn a_loop_of_links_is_refused() {
            let (_temp, root) = site();
            fs::remove_dir(logs(&root)).unwrap();
            symlink(logs(&root), logs(&root)).unwrap();
            assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
        }
    }

    // Windows links need Developer Mode or an administrator: each test
    // returns early where they cannot be made.
    #[cfg(windows)]
    #[test]
    fn storage_logs_linked_out_of_the_site_is_refused_on_windows() {
        let (temp, root) = site();
        let outside = temp.path().join("elsewhere");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("laravel.log"), "secret\n").unwrap();
        fs::remove_dir(logs(&root)).unwrap();
        if std::os::windows::fs::symlink_dir(&outside, logs(&root)).is_err() {
            return;
        }
        assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
    }

    #[cfg(windows)]
    #[test]
    fn a_log_file_that_is_a_link_is_refused_on_windows() {
        let (temp, root) = site();
        let secret = temp.path().join("hosts");
        fs::write(&secret, "127.0.0.1 secret\n").unwrap();
        if std::os::windows::fs::symlink_file(&secret, logs(&root).join("laravel.log")).is_err() {
            return;
        }
        assert_eq!(unsafe_start(&root), codes::SITE_LOG_UNSAFE);
    }
}
