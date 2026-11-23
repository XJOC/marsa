//! `cargo xtask check-rules`: the rules of `AGENTS.md` section 6 (Clarity)
//! that a script can check, so that they hold without a reviewer. CI runs it.
//!
//! Each check is a function of a file's path and text, with no I/O; [`run`]
//! reads the files. Every violation is one line, `path:line: rule: why`.

mod rust;
mod workflows;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::workspace_root;

/// Files whose code may pass [`rust::LIMIT`] lines, and why. An entry that
/// is no longer needed is a violation too, so the list stays true.
const LONG_FILES: &[(&str, &str)] = &[
    (
        "crates/app/src/cli/strings/errors.rs",
        "one table of texts: tables are split by domain, not by length",
    ),
    ("crates/core/src/types.rs", "to split by domain"),
    ("crates/core/src/databases/mod.rs", "to split by job"),
];

/// One broken rule.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Violation {
    /// From the repository root, with `/` on every system.
    path: String,
    /// From 1.
    line: usize,
    rule: &'static str,
    why: String,
}

impl Violation {
    fn new(path: &str, line: usize, rule: &'static str, why: String) -> Self {
        Self {
            path: path.to_owned(),
            line,
            rule,
            why,
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}",
            self.path, self.line, self.rule, self.why
        )
    }
}

/// Checks the repository and prints every violation.
pub(crate) fn run() -> Result<()> {
    let root = workspace_root()?;
    let mut rust_files = Vec::new();
    for top in ["crates", "xtask"] {
        rust_files_in(&root, &root.join(top), &mut rust_files)?;
    }
    rust_files.sort();
    let mut violations = Vec::new();
    let mut long = Vec::new();
    for (path, file) in &rust_files {
        let text = read(file)?;
        violations.extend(rust::check(path, &text, &mut long));
    }
    violations.extend(rust::check_exemptions(LONG_FILES, &long));
    let workflows_dir = root.join(".github").join("workflows");
    let mut workflows = Vec::new();
    for entry in fs::read_dir(&workflows_dir)
        .with_context(|| format!("cannot list {}", workflows_dir.display()))?
    {
        let file = entry?.path();
        if file.extension().is_some_and(|ext| ext == "yml") {
            workflows.push(file);
        }
    }
    workflows.sort();
    for file in &workflows {
        let path = relative(&root, file);
        let text = read(file)?;
        violations.extend(workflows::check(&path, &text, |filter| {
            root.join(filter).exists()
        }));
    }
    for violation in &violations {
        println!("{violation}");
    }
    if !violations.is_empty() {
        bail!("{} rule violations", violations.len());
    }
    println!(
        "check-rules: {} Rust files and {} workflows follow the rules",
        rust_files.len(),
        workflows.len()
    );
    Ok(())
}

/// Every `.rs` file under `dir`, with its path from `root`. Build output and
/// hidden folders are left out.
fn rust_files_in(root: &Path, dir: &Path, found: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files_in(root, &path, found)?;
            }
        } else if name.ends_with(".rs") {
            found.push((relative(root, &path), path));
        }
    }
    Ok(())
}

/// `path` from `root`, with `/` between the parts on every system.
fn relative(root: &Path, path: &Path) -> String {
    let parts: Vec<String> = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

fn read(file: &Path) -> Result<String> {
    fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_violation_reads_as_path_line_rule_why() {
        let violation = Violation::new("a/b.rs", 7, "task-id", "M4-13 in a comment".to_owned());
        assert_eq!(
            violation.to_string(),
            "a/b.rs:7: task-id: M4-13 in a comment"
        );
    }

    #[test]
    fn a_relative_path_uses_slashes() {
        let root = Path::new("root");
        let file = root.join("crates").join("core").join("lib.rs");
        assert_eq!(relative(root, &file), "crates/core/lib.rs");
    }

    #[test]
    fn every_exemption_has_a_reason() {
        for (path, why) in LONG_FILES {
            assert!(path.ends_with(".rs") && !why.is_empty(), "{path}");
        }
    }
}
