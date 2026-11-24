//! Following a site's log after its first read: one look at a time, each
//! started by the caller (every 500 ms while a client listens), so nothing
//! runs without a reader.

use marsa_core::fs::file_id;
use marsa_core::sitelog::{LineSplitter, MAX_STEP_BYTES, MAX_STEP_LINES};
use marsa_core::types::SiteLogLine;

use super::{Lines, LogError, Place, Tail, read_into};

/// How many other files a stream remembers where it stopped in, so a project
/// that writes two logs at once (`single` and `daily` in one `stack`) is not
/// read again from the start at each change of the newest.
const SEEN_FILES: usize = 8;

/// What one look at a followed log found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Step {
    /// The new lines.
    pub(crate) lines: Vec<SiteLogLine>,
    /// The file became shorter (`> laravel.log`, `logrotate` with
    /// `copytruncate`): it is read again from its start.
    pub(crate) truncated: bool,
    /// Another file is read: a newer daily file, the file of the same name
    /// made again, or the first file of a site that had none.
    pub(crate) switched: bool,
}

impl Tail {
    /// Looks once for new lines: at most [`MAX_STEP_BYTES`] and about
    /// [`MAX_STEP_LINES`] of them, the rest at the next look. No folder or
    /// no file yet is an empty step.
    ///
    /// # Errors
    ///
    /// [`LogError::Unsafe`], which ends a stream, or a read that failed
    /// before any byte, which waits for the next look (the file may be in
    /// the middle of a rotation).
    pub(crate) fn poll(&mut self) -> Result<Step, LogError> {
        let mut step = Step::default();
        if !self.dir.check()? {
            return Ok(step);
        }
        let Some(name) = self.dir.newest()? else {
            return Ok(step);
        };
        let Some((mut file, meta)) = self.dir.open(&name)? else {
            return Ok(step);
        };
        let id = file_id(&meta);
        let len = meta.len();
        let mut texts = Vec::new();
        let same = self
            .current
            .as_ref()
            .is_some_and(|place| place.name == name && place.id == id);
        if !same {
            self.switch(name, id, len, &mut texts);
            step.switched = true;
        } else if self.current.as_ref().is_some_and(|place| len < place.pos) {
            self.restart_at(0, &mut texts);
            step.truncated = true;
        }
        let pos = self.current.as_ref().map_or(0, |place| place.pos);
        let want = len.saturating_sub(pos).min(MAX_STEP_BYTES);
        if want > 0 {
            let read = read_into(
                &mut file,
                pos,
                want,
                &mut self.splitter,
                &mut texts,
                Lines::StopAfter(MAX_STEP_LINES),
            );
            // The bytes read are in the splitter, so `pos` moves past them
            // even when the read then failed.
            if let Some(place) = self.current.as_mut() {
                place.pos += read.bytes;
            }
            if let Some(err) = read.error
                && read.bytes == 0
                && texts.is_empty()
            {
                return Err(LogError::Io(err));
            }
        }
        step.lines = self.number(texts);
        Ok(step)
    }

    /// Moves to file `name`. A file read before in this stream goes on where
    /// it stopped; another one starts near its end, as at the start of the
    /// stream, so an old file that becomes the newest is not sent whole.
    fn switch(&mut self, name: String, id: Option<(u64, u64)>, len: u64, texts: &mut Vec<String>) {
        // A last line of the old file without a line end is kept, not lost.
        texts.extend(self.splitter.finish());
        self.splitter = LineSplitter::new(false);
        if let Some(old) = self.current.take() {
            self.seen.retain(|place| place.name != old.name);
            self.seen.push(old);
            if self.seen.len() > SEEN_FILES {
                self.seen.remove(0);
            }
        }
        let known = self
            .seen
            .iter()
            .position(|place| place.name == name && place.id == id && place.pos <= len);
        let pos = match known {
            Some(index) => self.seen.remove(index).pos,
            None => self.near_end(len, MAX_STEP_BYTES),
        };
        self.current = Some(Place { name, id, pos });
    }

    /// The file is read again from `pos`. A last line of the old read without
    /// a line end is kept, not lost.
    fn restart_at(&mut self, pos: u64, texts: &mut Vec<String>) {
        texts.extend(self.splitter.finish());
        self.splitter = LineSplitter::new(false);
        if let Some(place) = self.current.as_mut() {
            place.pos = pos;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::CHUNK;
    use super::super::tests::{append, logs, site, texts, touch};
    use super::*;
    use marsa_core::types::SiteLog;
    use std::fs;

    #[test]
    fn one_look_stops_after_about_a_step_of_lines() {
        let (_temp, root) = site();
        let file = logs(&root).join("laravel.log");
        fs::write(&file, "").unwrap();
        let (_, mut tail) = Tail::start(&root).unwrap();
        // Under `MAX_STEP_BYTES`, so only the line count stops a look: after
        // the piece that passes it.
        let total = 200_000;
        append(&file, &"\n".repeat(total));
        let first = tail.poll().unwrap().lines.len();
        assert_eq!(first, CHUNK);
        let mut sum = first;
        for _ in 0..4 {
            sum += tail.poll().unwrap().lines.len();
        }
        assert_eq!(sum, total);
    }

    #[test]
    fn two_files_written_in_turn_are_each_read_once() {
        let (_temp, root) = site();
        let dir = logs(&root);
        let single = dir.join("laravel.log");
        let daily = dir.join("laravel-2026-11-24.log");
        fs::write(&single, "single 1\n").unwrap();
        fs::write(&daily, "daily 1\n").unwrap();
        touch(&single, 2);
        touch(&daily, 1);
        let (log, mut tail) = Tail::start(&root).unwrap();
        assert_eq!(texts(&log.lines), ["single 1"]);
        append(&daily, "daily 2\n");
        touch(&daily, 3);
        assert_eq!(texts(&tail.poll().unwrap().lines), ["daily 1", "daily 2"]);
        append(&single, "single 2\n");
        touch(&single, 4);
        assert_eq!(texts(&tail.poll().unwrap().lines), ["single 2"]);
        append(&daily, "daily 3\n");
        touch(&daily, 5);
        assert_eq!(texts(&tail.poll().unwrap().lines), ["daily 3"]);
    }

    #[test]
    fn an_old_big_file_that_becomes_the_newest_is_read_near_its_end() {
        let (_temp, root) = site();
        let dir = logs(&root);
        let old = dir.join("laravel-2026-11-01.log");
        let line = format!("{}\n", "o".repeat(99));
        fs::write(&old, line.repeat(10_000)).unwrap();
        fs::write(dir.join("laravel-2026-11-24.log"), "today\n").unwrap();
        touch(&old, 1);
        touch(&dir.join("laravel-2026-11-24.log"), 2);
        let (_, mut tail) = Tail::start(&root).unwrap();
        touch(&old, 3);
        let step = tail.poll().unwrap();
        assert!(step.switched);
        let read: usize = step.lines.iter().map(|line| line.text.len() + 1).sum();
        assert!(read <= MAX_STEP_BYTES as usize, "{read}");
        assert!(step.lines.iter().all(|line| line.text.len() == 99));
    }

    #[test]
    fn new_lines_are_followed_and_a_partial_line_waits_for_its_end() {
        let (_temp, root) = site();
        let file = logs(&root).join("laravel.log");
        fs::write(&file, "one\n").unwrap();
        let (log, mut tail) = Tail::start(&root).unwrap();
        assert_eq!(texts(&log.lines), ["one"]);
        assert_eq!(tail.poll().unwrap(), Step::default());
        append(&file, "two\nthr");
        let step = tail.poll().unwrap();
        assert_eq!(texts(&step.lines), ["two"]);
        assert_eq!(step.lines[0].seq, 1);
        append(&file, "ee\n");
        assert_eq!(texts(&tail.poll().unwrap().lines), ["three"]);
    }

    #[test]
    fn a_truncated_file_is_read_again_from_its_start() {
        let (_temp, root) = site();
        let file = logs(&root).join("laravel.log");
        fs::write(&file, "one\ntwo\n").unwrap();
        let (_, mut tail) = Tail::start(&root).unwrap();
        fs::write(&file, "new\n").unwrap();
        let step = tail.poll().unwrap();
        assert!(step.truncated && !step.switched);
        assert_eq!(texts(&step.lines), ["new"]);
    }

    #[test]
    fn a_newer_daily_file_is_followed_from_its_start() {
        let (_temp, root) = site();
        let dir = logs(&root);
        fs::write(dir.join("laravel-2026-11-23.log"), "yesterday\n").unwrap();
        touch(&dir.join("laravel-2026-11-23.log"), 1);
        let (_, mut tail) = Tail::start(&root).unwrap();
        fs::write(dir.join("laravel-2026-11-24.log"), "today\n").unwrap();
        touch(&dir.join("laravel-2026-11-24.log"), 2);
        let step = tail.poll().unwrap();
        assert!(step.switched);
        assert_eq!(texts(&step.lines), ["today"]);
    }

    #[test]
    fn a_file_that_appears_later_is_read_from_its_start() {
        let temp = tempfile::tempdir().unwrap();
        let (log, mut tail) = Tail::start(temp.path()).unwrap();
        assert_eq!(log, SiteLog::default());
        assert_eq!(tail.poll().unwrap(), Step::default());
        fs::create_dir_all(logs(temp.path())).unwrap();
        fs::write(logs(temp.path()).join("laravel.log"), "first\n").unwrap();
        let step = tail.poll().unwrap();
        assert!(step.switched);
        assert_eq!(texts(&step.lines), ["first"]);
        assert_eq!(step.lines[0].seq, 0);
    }

    #[test]
    fn one_look_reads_at_most_a_step_and_the_rest_next_time() {
        let (_temp, root) = site();
        let file = logs(&root).join("laravel.log");
        fs::write(&file, "").unwrap();
        let (_, mut tail) = Tail::start(&root).unwrap();
        let line = format!("{}\n", "z".repeat(1023));
        append(&file, &line.repeat(300));
        let first = tail.poll().unwrap().lines.len();
        assert_eq!(first as u64, MAX_STEP_BYTES / 1024);
        assert_eq!(tail.poll().unwrap().lines.len(), 300 - first);
    }

    #[test]
    fn a_deleted_file_is_an_empty_step_not_an_error() {
        let (_temp, root) = site();
        let file = logs(&root).join("laravel.log");
        fs::write(&file, "one\n").unwrap();
        let (_, mut tail) = Tail::start(&root).unwrap();
        fs::remove_file(&file).unwrap();
        assert_eq!(tail.poll().unwrap(), Step::default());
    }

    // Links are made with `std::os::unix::fs::symlink`.
    #[cfg(unix)]
    mod links {
        use super::*;
        use std::os::unix::fs::symlink;

        #[test]
        fn a_followed_folder_swapped_for_a_link_ends_the_stream() {
            let (temp, root) = site();
            fs::write(logs(&root).join("laravel.log"), "one\n").unwrap();
            let (_, mut tail) = Tail::start(&root).unwrap();
            let outside = temp.path().join("elsewhere");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("laravel.log"), "secret\n").unwrap();
            fs::remove_dir_all(logs(&root)).unwrap();
            symlink(&outside, logs(&root)).unwrap();
            assert!(matches!(tail.poll(), Err(LogError::Unsafe(_))));
        }

        #[test]
        fn a_file_made_again_under_the_same_name_is_read_from_its_start() {
            let (_temp, root) = site();
            let file = logs(&root).join("laravel.log");
            fs::write(&file, "a long first line\n").unwrap();
            let (_, mut tail) = Tail::start(&root).unwrap();
            fs::rename(&file, logs(&root).join("laravel.log.1")).unwrap();
            fs::write(&file, "fresh and longer than the old one was\n").unwrap();
            let step = tail.poll().unwrap();
            assert!(step.switched);
            assert_eq!(
                texts(&step.lines),
                ["fresh and longer than the old one was"]
            );
        }
    }
}
