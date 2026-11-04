//! Runs the `marsa` binary with `--data-dir` in a temp folder. No test here
//! uses the network: the installer downloads the manifest before it checks
//! the key, so `manifest_no_trusted_key` cannot be reached offline. The
//! install test stops earlier, at the install lock.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Command, Output};

fn marsa(data_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_marsa"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .env_remove("MARSA_LOG")
        .output()
        .expect("run marsa")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
fn runtimes_on_an_empty_data_dir() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let out = marsa(tmp.path(), &["runtimes"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "PHP       none installed\n\
         Node.js   none installed\n\
         Composer  none installed\n"
    );
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn runtimes_lists_installed_versions() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let runtimes = tmp.path().join("runtimes");
    for dir in ["php/8.4.23", "php/8.3.27", "node/22.11.0", "composer/2.8.4"] {
        fs::create_dir_all(runtimes.join(dir)).expect("mkdir");
    }
    let out = marsa(tmp.path(), &["runtimes"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "PHP       8.3.27, 8.4.23\n\
         Node.js   22.11.0\n\
         Composer  2.8.4\n"
    );

    let out = marsa(tmp.path(), &["runtimes", "php"]);
    assert_eq!(text(&out.stdout), "PHP       8.3.27, 8.4.23\n");
}

#[test]
fn install_fails_with_its_code_when_another_install_runs() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let lock_dir = tmp.path().join("runtimes").join(".tmp");
    fs::create_dir_all(&lock_dir).expect("mkdir");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_dir.join("install.lock"))
        .expect("open the lock");
    lock.try_lock().expect("take the lock");

    let out = marsa(tmp.path(), &["install", "php", "8.4"]);
    drop(lock);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        "marsa: Another install is running. Wait for it to end, then try again. \
         [install_busy]\n"
    );
    assert!(!tmp.path().join("runtimes").join("php").exists());
}

#[test]
fn bad_arguments_exit_with_code_2() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let out = marsa(tmp.path(), &["install", "ruby"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("ruby"), "{}", text(&out.stderr));
}

#[test]
fn help_and_version_work() {
    let tmp = tempfile::tempdir().expect("temp dir");
    for args in [
        &["install", "--help"][..],
        &["runtimes", "--help"],
        &["--version"],
    ] {
        let out = marsa(tmp.path(), args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    }
    let out = marsa(tmp.path(), &["--version"]);
    assert_eq!(
        text(&out.stdout),
        format!("marsa {}\n", env!("CARGO_PKG_VERSION"))
    );
}
