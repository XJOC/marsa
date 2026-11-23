//! The checks of a Rust file: its size, task IDs in its comments, the
//! reason of each `#[allow]`, and the CLI's imports of the daemon.

use super::Violation;

/// The most lines of code in a file, and the most lines of inline tests.
pub(super) const LIMIT: usize = 800;

/// The checks of one file. A file whose code passes [`LIMIT`] goes into
/// `long`, with its count: [`check_exemptions`] decides.
pub(super) fn check(path: &str, text: &str, long: &mut Vec<(String, usize)>) -> Vec<Violation> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    if !is_test_file(path) {
        let tests = test_lines(&lines);
        let code = lines.len() - tests;
        if code > LIMIT {
            long.push((path.to_owned(), code));
        }
        if tests > LIMIT {
            found.push(Violation::new(
                path,
                1,
                "size",
                format!("{tests} lines of inline tests: move them to tests.rs (limit {LIMIT})"),
            ));
        }
    }
    let cli = path.starts_with("crates/app/src/cli/");
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        if let Some(comment) = comment(line) {
            for id in task_ids(comment) {
                found.push(Violation::new(
                    path,
                    number,
                    "task-id",
                    format!("{id} in a comment: say why, not which task (git blame keeps it)"),
                ));
            }
        }
        if is_allow(line) && !has_reason(&lines, index) {
            found.push(Violation::new(
                path,
                number,
                "allow-reason",
                "an #[allow] without a comment that gives the reason".to_owned(),
            ));
        }
        if cli && line.contains("crate::daemon") {
            found.push(Violation::new(
                path,
                number,
                "daemon-import",
                "the CLI is a client: shared code lives in marsa_core".to_owned(),
            ));
        }
    }
    found
}

/// A long file that is not in `exempt`, and an entry of `exempt` whose file
/// is no longer long.
pub(super) fn check_exemptions(
    exempt: &[(&str, &str)],
    long: &[(String, usize)],
) -> Vec<Violation> {
    let mut found = Vec::new();
    for (path, code) in long {
        if !exempt.iter().any(|(listed, _)| listed == path) {
            found.push(Violation::new(
                path,
                1,
                "size",
                format!("{code} lines of code: split the file by job (limit {LIMIT})"),
            ));
        }
    }
    for (listed, _) in exempt {
        if !long.iter().any(|(path, _)| path == listed) {
            found.push(Violation::new(
                listed,
                1,
                "size",
                format!("listed in LONG_FILES, but not longer than {LIMIT} lines: take it out"),
            ));
        }
    }
    found
}

/// A file of tests only: `tests.rs`, or a file under a `tests` folder.
fn is_test_file(path: &str) -> bool {
    path.ends_with("/tests.rs") || path.split('/').any(|part| part == "tests")
}

/// The lines of the file's inline test modules: an attribute block at
/// column 0 with a `#[cfg(...)]` that names `test`, then `mod NAME {` up to
/// the `}` at column 0, or `mod NAME;`.
fn test_lines(lines: &[&str]) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < lines.len() {
        let start = index;
        let mut is_test = false;
        while index < lines.len() && lines[index].starts_with("#[") {
            is_test |= lines[index].starts_with("#[cfg(") && names_test(lines[index]);
            index += 1;
        }
        if index == start {
            index += 1;
            continue;
        }
        let Some(line) = lines.get(index) else { break };
        let item = line
            .trim_start_matches("pub(crate) ")
            .trim_start_matches("pub ");
        if !is_test || !item.starts_with("mod ") {
            continue;
        }
        if item.ends_with(';') {
            count += index - start + 1;
            index += 1;
            continue;
        }
        let end = lines[index..]
            .iter()
            .position(|line| *line == "}")
            .map_or(lines.len() - 1, |offset| index + offset);
        count += end - start + 1;
        index = end + 1;
    }
    count
}

/// `test` as a word in a `cfg` attribute: `cfg(test)`, `cfg(all(test, unix))`.
fn names_test(attribute: &str) -> bool {
    attribute
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|word| word == "test")
}

/// The text after `//` on the line, if any. A `//` inside a string, such as
/// a URL, counts as a comment too: a task ID in a URL is unlikely.
fn comment(line: &str) -> Option<&str> {
    line.find("//").map(|at| &line[at + 2..])
}

/// The task IDs in `text`: `M`, a number, `-`, and a number or `D` and a
/// number; or `R-` or `S-` and a number. A small letter may end it. One
/// that goes on with `-` is something else: a Windows SID (`S-1-5-21`).
fn task_ids(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for (at, &byte) in bytes.iter().enumerate() {
        let starts_word = at == 0 || !is_word_byte(bytes[at - 1]);
        if !starts_word || !matches!(byte, b'M' | b'R' | b'S') {
            continue;
        }
        let mut end = at + 1;
        if byte == b'M' {
            end = digits(bytes, end).unwrap_or(at);
            if end == at {
                continue;
            }
        }
        if bytes.get(end) != Some(&b'-') {
            continue;
        }
        end += 1;
        if byte == b'M' && bytes.get(end) == Some(&b'D') {
            end += 1;
        }
        let Some(after) = digits(bytes, end) else {
            continue;
        };
        end = after;
        if bytes.get(end).is_some_and(u8::is_ascii_lowercase) {
            end += 1;
        }
        if bytes
            .get(end)
            .is_some_and(|&next| next.is_ascii_alphanumeric() || next == b'-')
        {
            continue;
        }
        found.push(&text[at..end]);
    }
    found
}

/// The index after one or more ASCII digits from `from`, if there is one.
fn digits(bytes: &[u8], from: usize) -> Option<usize> {
    let count = bytes[from.min(bytes.len())..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    (count > 0).then_some(from + count)
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
}

fn is_allow(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("#[allow(") || line.starts_with("#![allow(")
}

/// A comment on the `#[allow]` line, or above it past other attributes.
fn has_reason(lines: &[&str], index: usize) -> bool {
    if lines[index].contains("//") {
        return true;
    }
    lines[..index]
        .iter()
        .rev()
        .map(|line| line.trim_start())
        .find(|line| !line.starts_with("#[") && !line.starts_with("#!["))
        .is_some_and(|line| line.starts_with("//"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(path: &str, text: &str) -> Vec<&'static str> {
        check(path, text, &mut Vec::new())
            .into_iter()
            .map(|violation| violation.rule)
            .collect()
    }

    #[test]
    fn task_ids_are_found_in_their_forms() {
        let text =
            "M4-13, M1-06b, M5-D11, R-05 and S-09b; not SHA-256, R-target, UTF-8, x86_64-M1-2";
        assert_eq!(
            task_ids(text),
            ["M4-13", "M1-06b", "M5-D11", "R-05", "S-09b"]
        );
        assert!(task_ids("ADR 0026, RFC 9110 section 11.6.1, M4, R-, S-x").is_empty());
        assert!(task_ids("the SID S-1-5-21-1 or S-1-12-1-2").is_empty());
    }

    /// The comment marks are joined at run time, so that this file's own
    /// test texts are not comments with task IDs.
    #[test]
    fn a_task_id_in_a_comment_breaks_the_rule_and_in_code_does_not() {
        let slashes = "/".repeat(2);
        let line_comment = format!("let a = 1; {slashes} M4-13\n");
        assert_eq!(rules("crates/a/src/x.rs", &line_comment), ["task-id"]);
        let doc_comment = format!("{slashes}/ From R-05.\nfn a() {{}}\n");
        assert_eq!(rules("crates/a/src/x.rs", &doc_comment), ["task-id"]);
        assert!(rules("crates/a/src/x.rs", "const ID: &str = \"M4-13\";\n").is_empty());
    }

    #[test]
    fn an_allow_needs_a_comment_above_or_on_its_line() {
        let above = "// A reason.\n#[cfg(windows)]\n#[allow(dead_code)]\nfn a() {}\n";
        let same_line = "#[allow(dead_code)] // A reason.\nfn a() {}\n";
        let none = "fn b() {}\n#[allow(dead_code)]\nfn a() {}\n";
        let inner = "#![allow(unsafe_code)]\n";
        assert!(rules("crates/a/src/x.rs", above).is_empty());
        assert!(rules("crates/a/src/x.rs", same_line).is_empty());
        assert_eq!(rules("crates/a/src/x.rs", none), ["allow-reason"]);
        assert_eq!(rules("crates/a/src/x.rs", inner), ["allow-reason"]);
    }

    #[test]
    fn only_the_cli_may_not_import_the_daemon() {
        let text = "use crate::daemon::state::Daemon;\n";
        assert_eq!(
            rules("crates/app/src/cli/sites.rs", text),
            ["daemon-import"]
        );
        assert!(rules("crates/app/src/daemon/sites/api.rs", text).is_empty());
    }

    fn file(code: usize, tests: usize) -> String {
        let mut text = "fn a() {}\n".repeat(code);
        if tests > 0 {
            text.push_str("#[cfg(all(test, unix))]\nmod tests {\n");
            text.push_str(&"    fn t() {}\n".repeat(tests - 3));
            text.push_str("}\n");
        }
        text
    }

    #[test]
    fn the_inline_tests_are_counted_apart_from_the_code() {
        let lines: Vec<String> = file(10, 20).lines().map(str::to_owned).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        assert_eq!(test_lines(&lines), 20);
        let separate = "fn a() {}\n#[cfg(test)]\nmod tests;\n#[cfg(windows)]\nmod windows;\n";
        assert_eq!(test_lines(&separate.lines().collect::<Vec<_>>()), 2);
    }

    #[test]
    fn too_many_inline_tests_break_the_rule() {
        assert!(rules("crates/a/src/x.rs", &file(10, LIMIT)).is_empty());
        assert_eq!(rules("crates/a/src/x.rs", &file(10, LIMIT + 1)), ["size"]);
        // A file of tests only has no limit.
        assert!(rules("crates/a/src/x/tests.rs", &file(10, LIMIT + 1)).is_empty());
        assert!(rules("crates/a/tests/client.rs", &file(10, LIMIT + 1)).is_empty());
    }

    #[test]
    fn a_long_file_must_be_listed_and_a_listed_file_must_be_long() {
        let mut long = Vec::new();
        check("crates/a/src/x.rs", &file(LIMIT, LIMIT), &mut long);
        assert!(long.is_empty());
        check("crates/a/src/x.rs", &file(LIMIT + 1, 0), &mut long);
        assert_eq!(long, [("crates/a/src/x.rs".to_owned(), LIMIT + 1)]);

        let listed = [("crates/a/src/x.rs", "a table")];
        assert!(check_exemptions(&listed, &long).is_empty());
        let unlisted = check_exemptions(&[], &long);
        assert_eq!(unlisted.len(), 1);
        assert!(
            unlisted[0].why.contains("801 lines of code"),
            "{}",
            unlisted[0]
        );
        let stale = check_exemptions(&listed, &[]);
        assert_eq!(stale.len(), 1);
        assert!(stale[0].why.contains("take it out"), "{}", stale[0]);
    }
}
