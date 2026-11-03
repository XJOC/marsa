//! Integration test for the sample projects under `fixtures/`.
//!
//! The resolver and site detection tasks rely on these folders: every marker
//! file must exist, and the version files must hold the documented values.

use std::fs;
use std::path::{Path, PathBuf};

/// The repository root, two levels above this crate (`crates/core`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn fixture(path: &str) -> PathBuf {
    repo_root().join("fixtures").join(path)
}

/// Reads a version file and strips the line ending, the way the resolvers do
/// (architecture.md section 10: parsers of user files trim `\r`).
fn read_version(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

#[test]
fn every_fixture_file_exists() {
    let files = [
        "laravel/artisan",
        "laravel/composer.json",
        "laravel/.php-version",
        "laravel/public/index.php",
        "wordpress/wp-config.php",
        "wordpress/wp-load.php",
        "node/package.json",
        "node/.nvmrc",
        "no-version/index.php",
        "broken-version/.php-version",
    ];

    for file in files {
        assert!(fixture(file).is_file(), "missing fixture file: {file}");
    }
}

#[test]
fn laravel_pins_php_8_4() {
    assert_eq!(read_version(&fixture("laravel/.php-version")), "8.4");
}

#[test]
fn broken_version_stays_broken() {
    assert_eq!(read_version(&fixture("broken-version/.php-version")), "abc");
}
