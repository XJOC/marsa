//! Runs the real `marsa-shim` binary against a fake data folder.
//!
//! The child gets `HOME=<temp>` and no `XDG_DATA_HOME`, so the real
//! `app_data_dir()` points inside the temp folder. Nothing touches the user's
//! folders, the network, or the real `PATH`.

// Unix only: the shim `exec`s only on Unix (Windows is task M1-07), and the
// `directories` crate does not read `HOME` on Windows.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A fake program: prints its version and arguments, one per line, and exits
/// with 42 when its first argument is `fail`.
fn fake_program(version: &str) -> String {
    format!(
        "#!/bin/sh\necho \"version={version}\"\nfor a in \"$@\"; do echo \"arg=$a\"; done\n\
         [ \"$1\" = fail ] && exit 42\nexit 0\n"
    )
}

struct World {
    home: TempDir,
    project: PathBuf,
}

impl World {
    fn new() -> Self {
        let home = TempDir::new().expect("create temp home");
        // macOS gives the temp folder through a symlink (`/var` → `/private/var`).
        // `current_dir()` in the shim resolves it, so `root` in `sites.toml`
        // must be the resolved path too. Allowed here: Unix only, and the path
        // is not passed to another program.
        let project = fs::canonicalize(home.path())
            .expect("resolve temp home")
            .join("project");
        fs::create_dir(&project).expect("create project");
        let world = Self { home, project };
        world.install("php", "8.3.14");
        world.install("php", "8.4.3");
        world.install("node", "22.11.0");
        fs::create_dir(world.home.path().join("shims")).expect("create shims");
        world
    }

    /// Where `directories` puts the data folder for this `HOME`.
    fn data(&self) -> PathBuf {
        // The layout differs by OS; this is the only switch in the test.
        #[cfg(target_os = "macos")]
        let relative = "Library/Application Support/dev.marsa.Marsa";
        #[cfg(not(target_os = "macos"))]
        let relative = ".local/share/marsa";
        self.home.path().join(relative)
    }

    fn bin(&self, tool: &str, version: &str) -> PathBuf {
        self.data()
            .join("runtimes")
            .join(tool)
            .join(version)
            .join("bin")
    }

    fn install(&self, tool: &str, version: &str) {
        let bin = self.bin(tool, version);
        fs::create_dir_all(&bin).expect("create bin");
        let program = bin.join(tool);
        fs::write(&program, fake_program(version)).expect("write fake program");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// An empty script under `runtimes/`, such as `npm-cli.js`. The fake
    /// programs only print its path. Returns the full path.
    fn put_script(&self, relative: &str) -> PathBuf {
        let path = self.data().join("runtimes").join(relative);
        let dir = path.parent().expect("the script has a folder");
        fs::create_dir_all(dir).expect("create script folder");
        fs::write(&path, "").expect("write script");
        path
    }

    fn write_config(&self, file: &str, text: &str) {
        let dir = self.data().join("config");
        fs::create_dir_all(&dir).expect("create config dir");
        fs::write(dir.join(file), text).expect("write config");
    }

    /// Runs the shim through a symlink named `name`. A symlink, not a hard
    /// link: `target/` and the temp folder may be on different disks.
    fn run(&self, name: &str, args: &[&str]) -> Output {
        let link = self.home.path().join("shims").join(name);
        if !link.exists() {
            symlink(env!("CARGO_BIN_EXE_marsa-shim"), &link).expect("link shim");
        }
        self.run_path(&link, args)
    }

    fn run_path(&self, program: &Path, args: &[&str]) -> Output {
        Command::new(program)
            .args(args)
            .current_dir(&self.project)
            .env("HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .env_remove("MARSA_SHIM_DEPTH")
            // The shim must not need `PATH`; the fake programs use only shell
            // built-ins.
            .env("PATH", self.home.path().join("no-such-dir"))
            .output()
            .expect("start the shim")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_file_picks_the_version() {
    let world = World::new();
    fs::write(world.project.join(".php-version"), "8.3\n").expect("write version file");
    let output = world.run("php", &["-v"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "version=8.3.14\narg=-v\n");
}

#[test]
fn global_default_is_used_without_a_version_file() {
    let world = World::new();
    world.write_config("global.toml", "[php]\ndefault = \"8.4\"\n");
    let output = world.run("php", &[]);
    assert_eq!(stdout(&output), "version=8.4.3\n", "{}", stderr(&output));
}

#[test]
fn site_setting_beats_the_version_file() {
    let world = World::new();
    fs::write(world.project.join(".php-version"), "8.3").expect("write version file");
    let root = world.project.to_str().expect("temp paths are UTF-8");
    world.write_config(
        "sites.toml",
        &format!("[sites.shop]\nroot = '{root}'\nphp = \"8.4\"\n"),
    );
    let output = world.run("php", &[]);
    assert_eq!(stdout(&output), "version=8.4.3\n", "{}", stderr(&output));
}

#[test]
fn node_reads_nvmrc() {
    let world = World::new();
    fs::write(world.project.join(".nvmrc"), "22\n").expect("write .nvmrc");
    let output = world.run("node", &["-e", "1"]);
    assert_eq!(
        stdout(&output),
        "version=22.11.0\narg=-e\narg=1\n",
        "{}",
        stderr(&output)
    );
}

#[test]
fn arguments_and_exit_code_pass_through() {
    let world = World::new();
    fs::write(world.project.join(".php-version"), "8.4").expect("write version file");
    let output = world.run("php", &["fail", "a b", "", "c\"d"]);
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(
        stdout(&output),
        "version=8.4.3\narg=fail\narg=a b\narg=\narg=c\"d\n"
    );
}

#[test]
fn a_missing_version_exits_127() {
    let world = World::new();
    fs::write(world.project.join(".php-version"), "8.2").expect("write version file");
    let output = world.run("php", &[]);
    assert_eq!(output.status.code(), Some(127));
    let err = stderr(&output);
    assert!(err.starts_with("marsa: PHP 8.2 is not installed"), "{err}");
    assert!(
        err.contains("[not_installed]\nmarsa: hint: put PHP builds in "),
        "{err}"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn an_unknown_name_exits_127() {
    let world = World::new();
    let output = world.run_path(Path::new(env!("CARGO_BIN_EXE_marsa-shim")), &[]);
    assert_eq!(output.status.code(), Some(127));
    assert!(stderr(&output).contains("[shim_unknown_name]"));
}

#[test]
fn a_link_back_to_the_shim_stops_with_126() {
    let world = World::new();
    let bin = world.bin("php", "9.0.0");
    fs::create_dir_all(&bin).expect("create bin");
    symlink(env!("CARGO_BIN_EXE_marsa-shim"), bin.join("php")).expect("link loop");
    fs::write(world.project.join(".php-version"), "9").expect("write version file");

    let output = world.run("php", &[]);
    assert_eq!(output.status.code(), Some(126));
    assert!(
        stderr(&output).contains("[shim_loop]"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn npm_and_npx_run_their_script_through_node() {
    let world = World::new();
    fs::write(world.project.join(".nvmrc"), "22").expect("write .nvmrc");
    for (name, file) in [("npm", "npm-cli.js"), ("npx", "npx-cli.js")] {
        let script = world.put_script(&format!("node/22.11.0/lib/node_modules/npm/bin/{file}"));
        let output = world.run(name, &["run", "dev"]);
        let expected = format!(
            "version=22.11.0\narg={}\narg=run\narg=dev\n",
            script.display()
        );
        assert_eq!(stdout(&output), expected, "{}", stderr(&output));
    }
}

#[test]
fn composer_runs_the_highest_phar_through_the_project_php() {
    let world = World::new();
    fs::write(world.project.join(".php-version"), "8.3").expect("write version file");
    world.put_script("composer/2.7.0/composer.phar");
    let phar = world.put_script("composer/2.8.4/composer.phar");
    let output = world.run("composer", &["install"]);
    let expected = format!("version=8.3.14\narg={}\narg=install\n", phar.display());
    assert_eq!(stdout(&output), expected, "{}", stderr(&output));
}

#[test]
fn composer_without_a_phar_exits_127() {
    let world = World::new();
    fs::write(world.project.join(".php-version"), "8.3").expect("write version file");
    let output = world.run("composer", &[]);
    assert_eq!(output.status.code(), Some(127));
    let err = stderr(&output);
    assert!(
        err.contains("[shim_composer_not_installed]\nmarsa: hint: "),
        "{err}"
    );
}

#[test]
fn a_missing_npm_script_exits_127() {
    let world = World::new();
    fs::write(world.project.join(".nvmrc"), "22").expect("write .nvmrc");
    let output = world.run("npm", &[]);
    assert_eq!(output.status.code(), Some(127));
    let err = stderr(&output);
    assert!(
        err.contains("npm-cli.js is missing [shim_program_missing]"),
        "{err}"
    );
}
