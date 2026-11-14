//! Marsa's block in the hosts file, one `127.0.0.1 <name>` line per site:
//! on Windows (ADR 0018), and on Linux where systemd is older than 250 and
//! systemd-networkd runs, so `marsa0` cannot be used (ADR 0014, task
//! M4-18k). The hosts file has no wildcards, so the block changes with every
//! new site.
//!
//! The file belongs to the user and to other tools too. The helper changes
//! only the lines between its two marker lines and keeps every other byte as
//! it is: other entries, comments, line endings, and text that is not UTF-8.
//! If the markers do not pair up (a start without an end, an end without a
//! start), the file is left alone and the op fails with
//! `hosts_block_broken`: the helper never guesses where its block ends.
//!
//! The path is `<System32>\drivers\etc\hosts` on Windows, with `System32`
//! asked from Windows ([`System::system_dir`]), and `/etc/hosts` on Linux;
//! never from the request and never from the environment. The block's line
//! endings are those of the system: CRLF on Windows, LF on Linux. No
//! external command runs here.
//!
//! The file is replaced as a whole (`write_atomic`): on Linux it is root's,
//! mode 0644, as distributions ship it. An SELinux label is not copied; the
//! systems that use this path (Ubuntu 22.04, Debian 11) use AppArmor.
//!
//! The helper leaves the file alone, and says why, when it is a link or a
//! reparse point (the real hosts file never is one: a link could make the
//! helper copy another file into it), when it is marked read-only (the
//! owner locked it on purpose), and when the new file would be larger than
//! the helper reads back.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;

use crate::os::Os;
use crate::response::{OpResult, Outcome, code};
use crate::system::System;

/// The first and the last line of Marsa's block.
const BEGIN: &str = "# Marsa start";
const END: &str = "# Marsa end";
/// The second line of the block, for whoever opens the file.
const NOTE: &str = "# Managed by Marsa. Lines in this block are overwritten.";
/// The largest hosts file the helper reads. Block lists of ad domains make
/// hosts files of a few MiB; a larger file is left as it is.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Windows ignores the mode: the file gets the permissions of its folder.
/// On Linux everyone reads it, and only root changes it.
const MODE: u32 = 0o644;
const LINUX_HOSTS: &str = "/etc/hosts";

const SYNC: &str = "hosts_sync";
const REMOVE: &str = "hosts_remove";

/// `hosts_sync`: the block holds exactly `names`, sorted. An empty list
/// leaves an empty block (written only if it was not there yet).
pub fn sync(names: &[String], os: Os, system: &impl System) -> OpResult {
    // The request parser already allows `<label>.test` only. Checked again
    // here, because this text goes into a system file as it is: a line break
    // in a name would add a line of the caller's choice.
    if !names.iter().all(|name| is_test_name(name)) {
        let message = "a name is not <label>.test, so nothing was written".to_owned();
        return OpResult::failed(SYNC, code::REQUEST_INVALID, message);
    }
    apply(SYNC, Some(&block(names, os)), os, system)
}

/// `hosts_remove`: takes Marsa's block out. No block, or no hosts file, is
/// `unchanged`.
pub fn remove(os: Os, system: &impl System) -> OpResult {
    apply(REMOVE, None, os, system)
}

/// Reads the file, swaps Marsa's block for `block` (or for nothing), and
/// writes the file back only if a byte changed.
fn apply(op: &'static str, block: Option<&[u8]>, os: Os, system: &impl System) -> OpResult {
    let path = match hosts_path(os, system) {
        Ok(path) => path,
        Err(err) => {
            let message = format!("cannot find the Windows system folder: {}", err.kind());
            return OpResult::failed(op, code::HOSTS_READ, message);
        }
    };
    if system.is_link(&path) {
        // Some Linux systems link /etc/hosts (NixOS). A removal reads
        // through the link: with no block of Marsa's there, there is nothing
        // to take out, and an uninstall must not fail on it.
        let without_block = op == REMOVE
            && os == Os::Linux
            && system.read(&path, MAX_BYTES).is_ok_and(|bytes| {
                !bytes
                    .split(|byte| *byte == b'\n')
                    .any(|line| marker(line).is_some())
            });
        if without_block {
            return OpResult {
                op,
                outcome: Outcome::Unchanged,
            };
        }
        let message = format!(
            "{} is a link or a reparse point, so it was left as it is",
            path.display()
        );
        return OpResult::failed(op, code::HOSTS_READ, message);
    }
    let current = match system.read(&path, MAX_BYTES) {
        Ok(bytes) => bytes,
        // Windows works without a hosts file, and Linux resolves localhost
        // without one. `exists` does not follow links, so a dangling link is
        // not "no file": it is left alone.
        Err(err) if err.kind() == io::ErrorKind::NotFound && !system.exists(&path) => Vec::new(),
        Err(err) => {
            let message = format!(
                "cannot read {}: {}, so it was left as it is",
                path.display(),
                err.kind()
            );
            return OpResult::failed(op, code::HOSTS_READ, message);
        }
    };
    let Some(wanted) = rebuild(&current, block, eol(os)) else {
        let message = format!(
            "the lines \"{BEGIN}\" and \"{END}\" in {} do not pair up, so it was left as it is",
            path.display()
        );
        return OpResult::failed(op, code::HOSTS_BLOCK_BROKEN, message);
    };
    let outcome = if wanted == current {
        Outcome::Unchanged
    } else if system.is_read_only(&path) {
        let message = format!(
            "{} is marked read-only, so it was left as it is",
            path.display()
        );
        return OpResult::failed(op, code::HOSTS_WRITE, message);
    } else if wanted.len() as u64 > MAX_BYTES {
        // The next run could not read it back, and could not take the block
        // out again.
        let message = format!(
            "{} would be larger than {MAX_BYTES} bytes, so it was left as it is",
            path.display()
        );
        return OpResult::failed(op, code::HOSTS_WRITE, message);
    } else if let Err(err) = system.write_atomic(&path, &wanted, MODE) {
        let message = format!("cannot write {}: {}", path.display(), err.kind());
        return OpResult::failed(op, code::HOSTS_WRITE, message);
    } else {
        Outcome::Done
    };
    OpResult { op, outcome }
}

fn hosts_path(os: Os, system: &impl System) -> io::Result<PathBuf> {
    if os == Os::Linux {
        return Ok(PathBuf::from(LINUX_HOSTS));
    }
    Ok(system
        .system_dir()?
        .join("drivers")
        .join("etc")
        .join("hosts"))
}

/// `<label>.test`, with a label of `a-z`, `0-9`, and `-`. The same rule as
/// the request parser, without its limits on length and hyphens.
fn is_test_name(name: &str) -> bool {
    name.strip_suffix(".test").is_some_and(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

/// The line ending of the hosts file the system ships: CRLF on Windows, LF
/// on Linux.
fn eol(os: Os) -> &'static str {
    if os == Os::Windows { "\r\n" } else { "\n" }
}

/// Marsa's block for `names`: sorted, each once, with the line endings of
/// the system ([`eol`]).
fn block(names: &[String], os: Os) -> Vec<u8> {
    let eol = eol(os);
    let names: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let mut text = format!("{BEGIN}{eol}{NOTE}{eol}");
    for name in names {
        text.push_str("127.0.0.1 ");
        text.push_str(name);
        text.push_str(eol);
    }
    text.push_str(END);
    text.push_str(eol);
    text.into_bytes()
}

/// The UTF-8 byte order mark that some editors put at the start of a file.
const BOM: &[u8] = b"\xef\xbb\xbf";

/// A line that is exactly a marker, apart from its line ending and spaces
/// or tabs after it. A marker with anything before it is an ordinary line;
/// [`rebuild`] takes a byte order mark off the first line before asking.
fn marker(line: &[u8]) -> Option<&'static str> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let end = line
        .iter()
        .rposition(|byte| !matches!(byte, b'\r' | b' ' | b'\t'))
        .map_or(0, |last| last + 1);
    [BEGIN, END]
        .into_iter()
        .find(|marker| &line[..end] == marker.as_bytes())
}

/// `current` with every Marsa block taken out and `block` put where the
/// first one was, or at the end if there was none. Every byte outside the
/// blocks is kept. `None` if the markers do not pair up.
fn rebuild(current: &[u8], block: Option<&[u8]>, eol: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(current.len() + block.map_or(0, <[u8]>::len));
    let mut inside = false;
    let mut placed = false;
    // A byte order mark stays where it is, in front of whatever comes first.
    let (bom, current) = match current.strip_prefix(BOM) {
        Some(rest) => (BOM, rest),
        None => (&b""[..], current),
    };
    out.extend_from_slice(bom);
    for line in current.split_inclusive(|byte| *byte == b'\n') {
        match marker(line) {
            Some(BEGIN) if !inside => {
                inside = true;
                if !placed {
                    out.extend_from_slice(block.unwrap_or_default());
                    placed = true;
                }
            }
            Some(END) if inside => inside = false,
            // A start inside a block, or an end outside one.
            Some(_) => return None,
            // A line of the old block.
            None if inside => {}
            None => out.extend_from_slice(line),
        }
    }
    if inside {
        // A start without an end: where the block stops is unknown.
        return None;
    }
    if let (false, Some(block)) = (placed, block) {
        // A last line with no line break gets one, of the system's kind.
        if out.len() > bom.len() && !out.ends_with(b"\n") {
            out.extend_from_slice(eol.as_bytes());
        }
        out.extend_from_slice(block);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::ops;
    use crate::os::Os;
    use crate::request::Request;
    use crate::system::FakeSystem;

    const SYSTEM32: &str = r"C:\Windows\System32";
    /// The mode of the file a test starts with. Marsa never writes it, so
    /// the mode shows whether an op wrote the file.
    const OLD_MODE: u32 = 0o600;
    /// The start of the hosts file Windows ships: comments, CRLF.
    const SHIPPED: &str = "# Copyright (c) 1993-2009 Microsoft Corp.\r\n#\r\n\
                           # localhost name resolution is handled within DNS itself.\r\n\
                           #\t127.0.0.1       localhost\r\n#\t::1             localhost\r\n";

    fn path() -> PathBuf {
        Path::new(SYSTEM32)
            .join("drivers")
            .join("etc")
            .join("hosts")
    }

    /// A Windows machine, with `content` as its hosts file if given.
    fn windows(content: Option<&[u8]>) -> FakeSystem {
        let fake = FakeSystem {
            system_dir: Some(PathBuf::from(SYSTEM32)),
            ..FakeSystem::default()
        };
        if let Some(content) = content {
            let file = (content.to_vec(), OLD_MODE);
            fake.files.borrow_mut().insert(path(), file);
        }
        fake
    }

    fn file(fake: &FakeSystem) -> Option<(Vec<u8>, u32)> {
        fake.files.borrow().get(&path()).cloned()
    }

    fn text(fake: &FakeSystem) -> String {
        String::from_utf8(file(fake).unwrap().0).unwrap()
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn status(result: &OpResult) -> &'static str {
        match result.outcome {
            Outcome::Done => "done",
            Outcome::Unchanged => "unchanged",
            Outcome::Failed { code, .. } => code,
        }
    }

    fn block_of(list: &[&str]) -> String {
        String::from_utf8(block(&names(list), Os::Windows)).unwrap()
    }

    #[test]
    fn the_written_file() {
        let fake = windows(Some(SHIPPED.as_bytes()));
        let result = sync(&names(&["shop.test", "blog.test"]), Os::Windows, &fake);
        assert_eq!((result.op, status(&result)), (SYNC, "done"));
        insta::assert_snapshot!(text(&fake));
    }

    #[test]
    fn names_are_sorted_and_written_once() {
        let fake = windows(Some(b""));
        sync(&names(&["b.test", "a.test", "b.test"]), Os::Windows, &fake);
        assert_eq!(text(&fake), block_of(&["a.test", "b.test"]));
        assert_eq!(
            block_of(&["a.test", "b.test"]),
            format!("{BEGIN}\r\n{NOTE}\r\n127.0.0.1 a.test\r\n127.0.0.1 b.test\r\n{END}\r\n")
        );
    }

    #[test]
    fn the_same_names_again_write_nothing() {
        let content = format!("{SHIPPED}{}", block_of(&["blog.test", "shop.test"]));
        let fake = windows(Some(content.as_bytes()));
        // Another order in the request is still the same block.
        let result = sync(&names(&["shop.test", "blog.test"]), Os::Windows, &fake);
        assert_eq!(status(&result), "unchanged");
        assert_eq!(file(&fake), Some((content.into_bytes(), OLD_MODE)));
    }

    #[test]
    fn the_block_is_replaced_where_it_was_and_the_rest_keeps_every_byte() {
        // LF lines, a line that is not UTF-8, and no line break at the end.
        let before = b"10.0.0.1 nas\n\xff\xfe old tool\n".to_vec();
        let after = b"192.168.1.9 printer".to_vec();
        let old = block_of(&["gone.test"]);
        let content = [before.clone(), old.into_bytes(), after.clone()].concat();
        let fake = windows(Some(&content));
        let result = sync(&names(&["shop.test"]), Os::Windows, &fake);
        assert_eq!(status(&result), "done");
        let wanted = [before, block(&names(&["shop.test"]), Os::Windows), after].concat();
        assert_eq!(file(&fake), Some((wanted, MODE)));
    }

    #[test]
    fn a_file_without_a_final_line_break_gets_one_before_the_block() {
        let fake = windows(Some(b"10.0.0.1 nas"));
        sync(&names(&["shop.test"]), Os::Windows, &fake);
        let wanted = format!("10.0.0.1 nas\r\n{}", block_of(&["shop.test"]));
        assert_eq!(text(&fake), wanted);
    }

    #[test]
    fn no_names_leave_an_empty_block() {
        let fake = windows(Some(SHIPPED.as_bytes()));
        let result = sync(&[], Os::Windows, &fake);
        assert_eq!(status(&result), "done");
        let wanted = format!("{SHIPPED}{BEGIN}\r\n{NOTE}\r\n{END}\r\n");
        assert_eq!(text(&fake), wanted);
    }

    #[test]
    fn a_missing_hosts_file_is_created_by_sync_and_left_by_remove() {
        let fake = windows(None);
        assert_eq!(status(&remove(Os::Windows, &fake)), "unchanged");
        assert_eq!(file(&fake), None);
        assert_eq!(
            status(&sync(&names(&["shop.test"]), Os::Windows, &fake)),
            "done"
        );
        assert_eq!(text(&fake), block_of(&["shop.test"]));
    }

    #[test]
    fn remove_takes_only_the_block_out() {
        let content = format!(
            "{SHIPPED}{}10.0.0.1 nas\r\n",
            block_of(&["blog.test", "shop.test"])
        );
        let fake = windows(Some(content.as_bytes()));
        let result = remove(Os::Windows, &fake);
        assert_eq!((result.op, status(&result)), (REMOVE, "done"));
        assert_eq!(text(&fake), format!("{SHIPPED}10.0.0.1 nas\r\n"));
        // Nothing of Marsa's is left: the second call writes nothing.
        let before = file(&fake);
        assert_eq!(status(&remove(Os::Windows, &fake)), "unchanged");
        assert_eq!(file(&fake), before);
    }

    #[test]
    fn two_blocks_become_one_where_the_first_was() {
        let content = format!(
            "a\r\n{}b\r\n{}c\r\n",
            block_of(&["one.test"]),
            block_of(&["two.test"])
        );
        let fake = windows(Some(content.as_bytes()));
        sync(&names(&["shop.test"]), Os::Windows, &fake);
        let wanted = format!("a\r\n{}b\r\nc\r\n", block_of(&["shop.test"]));
        assert_eq!(text(&fake), wanted);
    }

    #[test]
    fn markers_that_do_not_pair_up_leave_the_file_alone() {
        let broken = [
            format!("{BEGIN}\r\n127.0.0.1 shop.test\r\n"),
            format!("127.0.0.1 shop.test\r\n{END}\r\n"),
            format!("{BEGIN}\r\n{BEGIN}\r\n{END}\r\n"),
            format!("{BEGIN}\r\n{END}\r\n{END}\r\n"),
            format!("{END}\r\n{BEGIN}\r\n"),
        ];
        for content in broken {
            for sync_op in [true, false] {
                let fake = windows(Some(content.as_bytes()));
                let result = if sync_op {
                    sync(&names(&["shop.test"]), Os::Windows, &fake)
                } else {
                    remove(Os::Windows, &fake)
                };
                assert_eq!(status(&result), code::HOSTS_BLOCK_BROKEN, "{content:?}");
                let Outcome::Failed { message, .. } = &result.outcome else {
                    panic!("not failed");
                };
                // The path, never the content.
                assert!(message.contains("hosts") && !message.contains("shop.test"));
                assert_eq!(
                    file(&fake),
                    Some((content.clone().into_bytes(), OLD_MODE)),
                    "{content:?}"
                );
            }
        }
    }

    #[test]
    fn only_an_exact_marker_line_is_a_marker() {
        // A line break, spaces, and tabs after the marker are allowed.
        for line in ["# Marsa start", "# Marsa start\r\n", "# Marsa start \t\r\n"] {
            assert_eq!(marker(line.as_bytes()), Some(BEGIN), "{line:?}");
        }
        assert_eq!(marker(b"# Marsa end\n"), Some(END));
        let ordinary = [
            " # Marsa start\r\n",
            "# marsa start\r\n",
            "# Marsa start here\r\n",
            "#Marsa start\r\n",
            "127.0.0.1 x # Marsa start\r\n",
            "\r\n",
            "",
        ];
        for line in ordinary {
            assert_eq!(marker(line.as_bytes()), None, "{line:?}");
        }
        // Such lines stay in the file like any other line.
        let content = " # Marsa start\r\n# Marsa end here\r\n";
        let fake = windows(Some(content.as_bytes()));
        assert_eq!(
            status(&sync(&names(&["shop.test"]), Os::Windows, &fake)),
            "done"
        );
        assert_eq!(
            text(&fake),
            format!("{content}{}", block_of(&["shop.test"]))
        );
    }

    #[test]
    fn a_name_that_is_not_a_test_name_writes_nothing() {
        let bad = [
            "shop.test\r\n1.2.3.4 bank.example",
            "shop.test 1.2.3.4",
            "sh\0op.test",
            "sh\top.test",
            "sh\u{f6}p.test",
            "Shop.test",
            "shop.com",
            ".test",
            "a.b.test",
            "",
        ];
        for name in bad {
            let fake = windows(Some(SHIPPED.as_bytes()));
            let result = sync(&names(&["ok.test", name]), Os::Windows, &fake);
            assert_eq!(status(&result), code::REQUEST_INVALID, "{name:?}");
            assert_eq!(
                file(&fake),
                Some((SHIPPED.as_bytes().to_vec(), OLD_MODE)),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_is_left_alone() {
        // Unreadable, larger than the limit, and a dangling link.
        let mut denied = windows(Some(SHIPPED.as_bytes()));
        denied.fail_reads.insert(path());
        let huge = windows(Some(&vec![b'#'; MAX_BYTES as usize + 1]));
        let mut link = windows(None);
        link.dangling.insert(path());
        for fake in [denied, huge, link] {
            let before = file(&fake);
            for result in [
                sync(&names(&["shop.test"]), Os::Windows, &fake),
                remove(Os::Windows, &fake),
            ] {
                assert_eq!(status(&result), code::HOSTS_READ);
            }
            assert_eq!(file(&fake), before);
        }
    }

    #[test]
    fn a_failed_write_is_hosts_write() {
        let content = format!("{SHIPPED}{}", block_of(&["old.test"]));
        let mut fake = windows(Some(content.as_bytes()));
        fake.fail_writes.insert(path());
        for result in [
            sync(&names(&["shop.test"]), Os::Windows, &fake),
            remove(Os::Windows, &fake),
        ] {
            assert_eq!(status(&result), code::HOSTS_WRITE);
        }
        assert_eq!(file(&fake), Some((content.into_bytes(), OLD_MODE)));
    }

    #[test]
    fn remove_of_a_file_that_is_only_the_block_leaves_it_empty() {
        let fake = windows(Some(block_of(&["shop.test"]).as_bytes()));
        assert_eq!(status(&remove(Os::Windows, &fake)), "done");
        assert_eq!(file(&fake), Some((Vec::new(), MODE)));
        // An empty file has nothing of Marsa's.
        assert_eq!(status(&remove(Os::Windows, &fake)), "unchanged");
    }

    /// An editor that saves "UTF-8 with BOM" puts three bytes in front of
    /// the first line. They stay, and the marker after them still counts.
    #[test]
    fn a_byte_order_mark_stays_and_does_not_hide_the_first_marker() {
        let content = format!("\u{feff}{}10.0.0.1 nas\r\n", block_of(&["old.test"]));
        let fake = windows(Some(content.as_bytes()));
        assert_eq!(
            status(&sync(&names(&["shop.test"]), Os::Windows, &fake)),
            "done"
        );
        let wanted = format!("\u{feff}{}10.0.0.1 nas\r\n", block_of(&["shop.test"]));
        assert_eq!(text(&fake), wanted);
        assert_eq!(status(&remove(Os::Windows, &fake)), "done");
        assert_eq!(text(&fake), "\u{feff}10.0.0.1 nas\r\n");
        // A file of only a mark gets the block after it, with no line break.
        let fake = windows(Some(BOM));
        sync(&names(&["shop.test"]), Os::Windows, &fake);
        assert_eq!(text(&fake), format!("\u{feff}{}", block_of(&["shop.test"])));
    }

    /// The real hosts file is never a link: one could make the helper copy
    /// another file into it.
    #[test]
    fn a_hosts_file_that_is_a_link_is_left_alone() {
        let mut fake = windows(Some(SHIPPED.as_bytes()));
        fake.links.insert(path());
        for result in [
            sync(&names(&["shop.test"]), Os::Windows, &fake),
            remove(Os::Windows, &fake),
        ] {
            assert_eq!(status(&result), code::HOSTS_READ);
        }
        assert_eq!(file(&fake), Some((SHIPPED.as_bytes().to_vec(), OLD_MODE)));
    }

    #[test]
    fn a_read_only_hosts_file_is_left_alone_unless_nothing_changes() {
        let content = format!("{SHIPPED}{}", block_of(&["shop.test"]));
        let mut fake = windows(Some(content.as_bytes()));
        fake.read_only.insert(path());
        assert_eq!(
            status(&sync(&names(&["shop.test"]), Os::Windows, &fake)),
            "unchanged"
        );
        for result in [
            sync(&names(&["blog.test"]), Os::Windows, &fake),
            remove(Os::Windows, &fake),
        ] {
            assert_eq!(status(&result), code::HOSTS_WRITE);
        }
        assert_eq!(file(&fake), Some((content.into_bytes(), OLD_MODE)));
    }

    /// The helper never writes a file it could not read back.
    #[test]
    fn a_result_larger_than_the_read_limit_is_not_written() {
        let filler = vec![b'#'; MAX_BYTES as usize - 10];
        let fake = windows(Some(&filler));
        let result = sync(&names(&["shop.test"]), Os::Windows, &fake);
        assert_eq!(status(&result), code::HOSTS_WRITE);
        assert_eq!(file(&fake), Some((filler, OLD_MODE)));
    }

    /// Off Windows there is no system folder to ask for.
    #[test]
    fn without_the_system_folder_nothing_is_read_or_written() {
        let fake = FakeSystem::default();
        for result in [
            sync(&names(&["shop.test"]), Os::Windows, &fake),
            remove(Os::Windows, &fake),
        ] {
            assert_eq!(status(&result), code::HOSTS_READ);
        }
        assert!(fake.files.borrow().is_empty());
    }

    /// A real request, through the parser and `ops::apply`.
    #[test]
    fn a_request_runs_both_ops_on_windows() {
        let raw = json!({ "version": 1, "ops": [
            { "op": "hosts_sync", "names": ["shop.test", "blog.test"] },
            { "op": "hosts_remove" },
        ]})
        .to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let fake = windows(Some(SHIPPED.as_bytes()));
        let results = ops::apply(&request, Os::Windows, &fake);
        let seen: Vec<_> = results.iter().map(|r| (r.op, status(r))).collect();
        assert_eq!(seen, [(SYNC, "done"), (REMOVE, "done")]);
        assert_eq!(text(&fake), SHIPPED);
        assert!(fake.log.borrow().is_empty());
    }

    /// The hosts file a Debian or Ubuntu system ships: LF.
    const DEBIAN_HOSTS: &str = "127.0.0.1\tlocalhost\n127.0.1.1\trunner\n\n# The following \
                                lines are desirable for IPv6 capable hosts\n::1     \
                                ip6-localhost ip6-loopback\n";

    /// Linux: `/etc/hosts`, LF line endings, every other byte kept, and a
    /// request runs both ops (task M4-18k).
    #[test]
    fn on_linux_the_block_goes_into_etc_hosts_with_lf() {
        let fake = FakeSystem::default();
        let path = PathBuf::from("/etc/hosts");
        let file = (DEBIAN_HOSTS.as_bytes().to_vec(), OLD_MODE);
        fake.files.borrow_mut().insert(path.clone(), file);
        let result = sync(&names(&["shop.test", "blog.test"]), Os::Linux, &fake);
        assert_eq!(status(&result), "done");
        let (bytes, mode) = fake.files.borrow()[&path].clone();
        assert_eq!(mode, 0o644);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(
            text,
            format!(
                "{DEBIAN_HOSTS}{BEGIN}\n{NOTE}\n127.0.0.1 blog.test\n127.0.0.1 shop.test\n{END}\n"
            )
        );
        assert!(!text.contains('\r'));
        let same = sync(&names(&["blog.test", "shop.test"]), Os::Linux, &fake);
        assert_eq!(status(&same), "unchanged");

        let raw = json!({ "version": 1, "ops": [{ "op": "hosts_remove" }] }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let results = ops::apply(&request, Os::Linux, &fake);
        assert_eq!(status(&results[0]), "done");
        assert_eq!(fake.files.borrow()[&path].0, DEBIAN_HOSTS.as_bytes());
        assert!(fake.log.borrow().is_empty());
    }

    /// A link at `/etc/hosts` with no block of Marsa's (NixOS): a removal is
    /// `unchanged`, so an uninstall does not fail on it; with a block, it is
    /// still left alone.
    #[test]
    fn a_linked_hosts_file_without_a_block_is_unchanged_by_remove() {
        let mut fake = linux(Some(DEBIAN_HOSTS.as_bytes()));
        fake.links.insert(PathBuf::from("/etc/hosts"));
        assert_eq!(status(&remove(Os::Linux, &fake)), "unchanged");
        let with_block = format!("{DEBIAN_HOSTS}{BEGIN}\n{NOTE}\n{END}\n");
        let mut fake = linux(Some(with_block.as_bytes()));
        fake.links.insert(PathBuf::from("/etc/hosts"));
        assert_eq!(status(&remove(Os::Linux, &fake)), code::HOSTS_READ);
        assert_eq!(etc_hosts(&fake).unwrap(), with_block.as_bytes());
    }

    /// A link at `/etc/hosts` is left alone on Linux too.
    #[test]
    fn on_linux_a_hosts_file_that_is_a_link_is_left_alone() {
        let mut fake = FakeSystem::default();
        let path = PathBuf::from("/etc/hosts");
        fake.files
            .borrow_mut()
            .insert(path.clone(), (DEBIAN_HOSTS.as_bytes().to_vec(), OLD_MODE));
        fake.links.insert(path.clone());
        let result = sync(&names(&["shop.test"]), Os::Linux, &fake);
        assert_eq!(status(&result), code::HOSTS_READ);
        assert_eq!(fake.files.borrow()[&path].0, DEBIAN_HOSTS.as_bytes());
    }

    /// Linux: `/etc/hosts` in a fake, with `content` if given.
    fn linux(content: Option<&[u8]>) -> FakeSystem {
        let fake = FakeSystem::default();
        if let Some(content) = content {
            let file = (content.to_vec(), OLD_MODE);
            fake.files
                .borrow_mut()
                .insert(PathBuf::from("/etc/hosts"), file);
        }
        fake
    }

    fn etc_hosts(fake: &FakeSystem) -> Option<Vec<u8>> {
        let files = fake.files.borrow();
        files
            .get(Path::new("/etc/hosts"))
            .map(|(bytes, _)| bytes.clone())
    }

    /// A last line with no line break gets an LF on Linux, never a CR, and
    /// the removal keeps that LF.
    #[test]
    fn on_linux_a_file_without_a_final_newline_gets_lf() {
        let fake = linux(Some(b"127.0.0.1 localhost"));
        assert_eq!(
            status(&sync(&names(&["shop.test"]), Os::Linux, &fake)),
            "done"
        );
        let text = String::from_utf8(etc_hosts(&fake).unwrap()).unwrap();
        assert!(
            text.starts_with("127.0.0.1 localhost\n# Marsa start\n"),
            "{text:?}"
        );
        assert!(!text.contains('\r'));
        assert_eq!(status(&remove(Os::Linux, &fake)), "done");
        assert_eq!(etc_hosts(&fake).unwrap(), b"127.0.0.1 localhost\n");
    }

    /// A missing `/etc/hosts`: sync makes one with the block, and remove
    /// leaves nothing.
    #[test]
    fn on_linux_a_missing_hosts_file_is_made_by_sync_only() {
        let fake = linux(None);
        assert_eq!(status(&remove(Os::Linux, &fake)), "unchanged");
        assert_eq!(etc_hosts(&fake), None);
        assert_eq!(
            status(&sync(&names(&["shop.test"]), Os::Linux, &fake)),
            "done"
        );
        let text = String::from_utf8(etc_hosts(&fake).unwrap()).unwrap();
        assert_eq!(
            text,
            format!("{BEGIN}\n{NOTE}\n127.0.0.1 shop.test\n{END}\n")
        );
    }

    /// The refusals on Linux: a bad name, a read-only file, and markers that
    /// do not pair up. The file is never touched.
    #[test]
    fn on_linux_the_refusals_leave_etc_hosts_alone() {
        let fake = linux(Some(DEBIAN_HOSTS.as_bytes()));
        let bad = sync(&names(&["shop.test\n1.2.3.4 bank.com"]), Os::Linux, &fake);
        assert_eq!(status(&bad), code::REQUEST_INVALID);
        let mut locked = linux(Some(DEBIAN_HOSTS.as_bytes()));
        locked.read_only.insert(PathBuf::from("/etc/hosts"));
        assert_eq!(
            status(&sync(&names(&["shop.test"]), Os::Linux, &locked)),
            code::HOSTS_WRITE
        );
        let broken_text = format!("{DEBIAN_HOSTS}{BEGIN}\n127.0.0.1 shop.test\n");
        let broken = linux(Some(broken_text.as_bytes()));
        for result in [
            sync(&names(&["shop.test"]), Os::Linux, &broken),
            remove(Os::Linux, &broken),
        ] {
            assert_eq!(status(&result), code::HOSTS_BLOCK_BROKEN);
        }
        assert_eq!(etc_hosts(&fake).unwrap(), DEBIAN_HOSTS.as_bytes());
        assert_eq!(etc_hosts(&locked).unwrap(), DEBIAN_HOSTS.as_bytes());
        assert_eq!(etc_hosts(&broken).unwrap(), broken_text.as_bytes());
    }
}
