//! The check of a workflow: every path of its `paths:` and `paths-ignore:`
//! filters exists. A filter that names a moved file never matches, so the
//! workflow stops running on the pull requests that change that code.

use super::Violation;

/// The filters of `text` whose path does not exist. `exists` gets the part
/// of the filter before its first glob character, without a final `/`.
pub(super) fn check(path: &str, text: &str, exists: impl Fn(&str) -> bool) -> Vec<Violation> {
    let mut found = Vec::new();
    let mut in_filter = false;
    for (index, line) in text.lines().enumerate() {
        let item = line.trim();
        if item == "paths:" || item == "paths-ignore:" {
            in_filter = true;
            continue;
        }
        if !in_filter || item.is_empty() || item.starts_with('#') {
            continue;
        }
        let Some(filter) = item.strip_prefix("- ") else {
            in_filter = false;
            continue;
        };
        let filter = filter.trim().trim_matches(|c| c == '"' || c == '\'');
        let filter = filter.strip_prefix('!').unwrap_or(filter);
        let fixed = filter
            .split(['*', '?', '['])
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        if !fixed.is_empty() && !exists(fixed) {
            found.push(Violation::new(
                path,
                index + 1,
                "workflow-paths",
                format!("{filter}: no such path, so the filter never matches"),
            ));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKFLOW: &str = r#"on:
  pull_request:
    paths:
      - "crates/app/src/daemon/databases/**"
      # A comment.
      - 'crates/core/src/lib.rs'
      - "!crates/core/src/gone.rs"
      - "**.md"
  push:
    branches: [main]
jobs:
  run:
    steps:
      - "crates/not/a/filter.rs"
"#;

    #[test]
    fn a_filter_must_name_a_path_that_exists() {
        let exists = |path: &str| path == "crates/core/src/lib.rs";
        let found: Vec<String> = check(".github/workflows/x.yml", WORKFLOW, exists)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            found,
            [
                ".github/workflows/x.yml:4: workflow-paths: crates/app/src/daemon/databases/**: \
                 no such path, so the filter never matches",
                ".github/workflows/x.yml:7: workflow-paths: crates/core/src/gone.rs: \
                 no such path, so the filter never matches",
            ]
        );
    }
}
