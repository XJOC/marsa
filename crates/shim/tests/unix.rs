//! Runs the real `marsa-shim` binary against a fake data folder.
//!
//! The child gets `HOME=<temp>` and no `XDG_DATA_HOME`, so the real
//! `app_data_dir()` points inside the temp folder. Nothing touches the user's
//! folders, the network, or the real `PATH`.

// Unix only: the shim `exec`s only on Unix (the Windows shim does not exec), and the
// `directories` crate does not read `HOME` on Windows.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use marsa_core::databases::clients::SHIM_MARK;
use marsa_core::php::{PhpFiles, php_ini};
use marsa_core::recipes::Platform;
use semver::Version;
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
        self.put_program(tool, version, tool, &fake_program(version));
    }

    /// `runtimes/<runtime>/<version>/bin/<program>`, made runnable.
    fn put_program(&self, runtime: &str, version: &str, program: &str, text: &str) {
        let bin = self.bin(runtime, version);
        fs::create_dir_all(&bin).expect("create bin");
        let path = bin.join(program);
        fs::write(&path, text).expect("write fake program");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// A fake database client of `runtime` at `version`.
    fn install_client(&self, runtime: &str, version: &str, program: &str) {
        self.put_program(runtime, version, program, &fake_client(version));
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
        self.run_with(name, args, &[])
    }

    fn run_path(&self, program: &Path, args: &[&str]) -> Output {
        self.command(program, args)
            .output()
            .expect("start the shim")
    }

    /// Runs the shim through a symlink named `name`, with `vars` set in its
    /// environment.
    fn run_with(&self, name: &str, args: &[&str], vars: &[(&str, &str)]) -> Output {
        let link = self.home.path().join("shims").join(name);
        if !link.exists() {
            symlink(env!("CARGO_BIN_EXE_marsa-shim"), &link).expect("link shim");
        }
        let mut command = self.command(&link, args);
        command.envs(vars.iter().copied());
        command.output().expect("start the shim")
    }

    fn command(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&self.project)
            .env("HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .env_remove("MARSA_SHIM_DEPTH")
            // The shim sets it for PHP; a developer's own must not leak in.
            .env_remove("PHPRC")
            // The shim must not need `PATH`; the fake programs use only shell
            // built-ins.
            .env("PATH", self.home.path().join("no-such-dir"));
        // The shim adds these for the clients; a developer's own must not
        // leak in.
        for var in SERVER_VARS {
            command.env_remove(var);
        }
        command
    }
}

/// The variables the shim may add for a database client.
const SERVER_VARS: [&str; 5] = ["PGHOST", "PGPORT", "PGUSER", "MYSQL_HOST", "MYSQL_TCP_PORT"];

/// A fake database client: prints its version, its arguments, then each of
/// [`SERVER_VARS`] as `NAME=value`, or `NAME unset`.
fn fake_client(version: &str) -> String {
    let mut text = fake_program(version).replace("exit 0\n", "");
    for var in SERVER_VARS {
        text.push_str(&format!(
            "if [ \"${{{var}+set}}\" = set ]; then echo \"{var}=${var}\"; else echo \"{var} unset\"; fi\n"
        ));
    }
    text.push_str("exit 0\n");
    text
}

/// What [`fake_client`] prints for its variables, from `NAME=value` pairs:
/// every other variable is unset.
fn vars_out(set: &[(&str, &str)]) -> String {
    SERVER_VARS
        .iter()
        .map(|var| match set.iter().find(|(name, _)| name == var) {
            Some((_, value)) => format!("{var}={value}\n"),
            None => format!("{var} unset\n"),
        })
        .collect()
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
        err.contains("[not_installed]\nmarsa: hint: run \"marsa install php 8.2\"\n"),
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

/// A fake `php -m`: it lists Xdebug when the `php.ini` that `PHPRC` names has
/// a `zend_extension` line whose file exists, as PHP does. Shell built-ins
/// only.
const FAKE_PHP_M: &str = r#"#!/bin/sh
echo "[Zend Modules]"
[ -f "$PHPRC" ] || exit 0
while IFS= read -r line; do
  case "$line" in
    'zend_extension = "'*'"')
      file=${line#*\"}
      file=${file%\"}
      [ -f "$file" ] && echo "Xdebug"
      ;;
  esac
done < "$PHPRC"
exit 0
"#;

/// The daemon writes `php/<minor>/php.ini` with
/// `marsa_core::php::php_ini`; the shim points `PHPRC` at that file. So this
/// test writes the file the same way, and runs `php -m` through the shim.
#[test]
fn php_m_lists_xdebug_once_the_php_ini_of_its_line_loads_it() {
    let world = World::new();
    // The shim reads `global.toml` too: the new key must not stop it.
    world.write_config("global.toml", "[php]\ndefault = \"8.4\"\nxdebug = true\n");
    let php = world.bin("php", "8.4.3").join("php");
    fs::write(&php, FAKE_PHP_M).expect("write the fake php");

    let version = Version::parse("8.4.3").expect("a valid version");
    let mut files = PhpFiles::new(&world.data(), &version);
    fs::create_dir_all(&files.ext_dir).expect("create ext");
    fs::write(files.xdebug_so(), "").expect("write xdebug.so");
    let platform = Platform::current().expect("a system Marsa has PHP builds for");
    let write_ini = |files: &PhpFiles| {
        let dir = files.ini_file.parent().expect("php.ini has a folder");
        fs::create_dir_all(dir).expect("create the config folder");
        let text = php_ini(platform, files).expect("render php.ini");
        fs::write(&files.ini_file, text).expect("write php.ini");
    };
    let php_m = || {
        let output = world.run("php", &["-m"]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        stdout(&output)
    };

    // No `php.ini` yet: PHP runs without Xdebug.
    assert_eq!(php_m(), "[Zend Modules]\n");

    // `marsa xdebug on`: the file loads `ext/xdebug.so` of this PHP.
    files.xdebug = Some(files.xdebug_so());
    write_ini(&files);
    assert_eq!(php_m(), "[Zend Modules]\nXdebug\n");

    // `marsa xdebug off`: the same file without Xdebug.
    files.xdebug = None;
    write_ini(&files);
    assert_eq!(php_m(), "[Zend Modules]\n");
}

/// `psql` with PostgreSQL 18 installed and nothing in `global.toml`: the
/// newest version, on `127.0.0.1:5432`, as `root`.
#[test]
fn psql_reaches_marsas_postgres_as_root() {
    let world = World::new();
    world.install_client("postgres", "17.9.0", "psql");
    world.install_client("postgres", "18.7.0", "psql");
    // A version file in the project plays no part.
    fs::write(world.project.join(".php-version"), "8.3").expect("write version file");
    let output = world.run("psql", &["-c", "select 1"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let vars = [
        ("PGHOST", "127.0.0.1"),
        ("PGPORT", "5432"),
        ("PGUSER", "root"),
    ];
    let want = format!("version=18.7.0\narg=-c\narg=select 1\n{}", vars_out(&vars));
    assert_eq!(stdout(&output), want);
}

/// `PGHOST=db.example psql`: the user's variables stay as they are, an
/// empty one too, and only the missing ones are added.
#[test]
fn a_variable_the_user_set_is_left_alone() {
    let world = World::new();
    world.install_client("postgres", "18.7.0", "pg_dump");
    let set = [("PGHOST", "db.example"), ("PGUSER", "")];
    let output = world.run_with("pg_dump", &["shop"], &set);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let vars = [("PGHOST", "db.example"), ("PGPORT", "5432"), ("PGUSER", "")];
    let want = format!("version=18.7.0\narg=shop\n{}", vars_out(&vars));
    assert_eq!(stdout(&output), want);
}

/// The `default` line and the port of the server's table in `global.toml`.
#[test]
fn the_line_and_the_port_come_from_global_toml() {
    let world = World::new();
    world.install_client("postgres", "17.9.0", "pg_restore");
    world.install_client("postgres", "18.7.0", "pg_restore");
    world.write_config("global.toml", "[postgres]\ndefault = \"17\"\nport = 5433\n");
    let output = world.run("pg_restore", &[]);
    let vars = [
        ("PGHOST", "127.0.0.1"),
        ("PGPORT", "5433"),
        ("PGUSER", "root"),
    ];
    let want = format!("version=17.9.0\n{}", vars_out(&vars));
    assert_eq!(stdout(&output), want, "{}", stderr(&output));
}

/// `php artisan schema:dump` runs `mysqldump`: it reaches Marsa's MySQL over
/// TCP, never the default socket in `/tmp`.
#[test]
fn mysql_clients_reach_marsas_mysql_over_tcp() {
    let world = World::new();
    world.write_config("global.toml", "[mysql]\nport = 3307\n");
    for program in ["mysql", "mysqldump", "mysqladmin"] {
        world.install_client("mysql", "8.4.13", program);
        let output = world.run(program, &["--user=root", "laravel"]);
        let vars = [("MYSQL_HOST", "127.0.0.1"), ("MYSQL_TCP_PORT", "3307")];
        let want = format!(
            "version=8.4.13\narg=--user=root\narg=laravel\n{}",
            vars_out(&vars)
        );
        assert_eq!(stdout(&output), want, "{program}: {}", stderr(&output));
    }
}

/// `redis-cli ping` runs Valkey's `valkey-cli`, which prints `PONG`. No
/// variable and no argument is added.
#[test]
fn redis_cli_runs_valkey_cli() {
    let world = World::new();
    let fake = "#!/bin/sh\n[ \"$1\" = ping ] && echo PONG && exit 0\nexit 3\n";
    world.put_program("valkey", "8.1.11", "valkey-cli", fake);
    for name in ["redis-cli", "valkey-cli"] {
        let output = world.run(name, &["ping"]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert_eq!(stdout(&output), "PONG\n", "{name}");
    }
    world.install_client("valkey", "8.1.11", "valkey-cli");
    let output = world.run("redis-cli", &["-p", "6380"]);
    let want = format!("version=8.1.11\narg=-p\narg=6380\n{}", vars_out(&[]));
    assert_eq!(stdout(&output), want, "{}", stderr(&output));
}

/// A Valkey on another port than 6379: `valkey-cli` and `redis-cli` get
/// `-p <port>` first, unless the user already says where to connect.
#[test]
fn valkey_cli_goes_to_the_port_of_global_toml() {
    let world = World::new();
    world.write_config("global.toml", "[valkey]\nport = 6380\n");
    world.install_client("valkey", "8.1.11", "valkey-cli");
    for name in ["valkey-cli", "redis-cli"] {
        let output = world.run(name, &["ping"]);
        let want = format!(
            "version=8.1.11\narg=-p\narg=6380\narg=ping\n{}",
            vars_out(&[])
        );
        assert_eq!(stdout(&output), want, "{name}: {}", stderr(&output));
    }
    // The user's own target is kept.
    for given in [
        ["-p", "6390"].as_slice(),
        ["--port=6390"].as_slice(),
        ["-p6390"].as_slice(),
    ] {
        let output = world.run("valkey-cli", given);
        let args: String = given.iter().map(|arg| format!("arg={arg}\n")).collect();
        let want = format!("version=8.1.11\n{args}{}", vars_out(&[]));
        assert_eq!(stdout(&output), want, "{given:?}: {}", stderr(&output));
    }
    // Valkey's own port: nothing is added.
    world.write_config("global.toml", "[valkey]\nport = 6379\n");
    let output = world.run("valkey-cli", &["ping"]);
    let want = format!("version=8.1.11\narg=ping\n{}", vars_out(&[]));
    assert_eq!(stdout(&output), want, "{}", stderr(&output));
}

/// `psql` with no PostgreSQL installed: the shim's usual message, which
/// names the install.
#[test]
fn a_client_without_its_server_exits_127_with_the_install() {
    let world = World::new();
    let output = world.run("psql", &[]);
    assert_eq!(output.status.code(), Some(127));
    let err = stderr(&output);
    assert!(
        err.starts_with("marsa: PostgreSQL is not installed"),
        "{err}"
    );
    assert!(
        err.ends_with("[shim_server_not_installed]\nmarsa: hint: run \"marsa install postgres\"\n"),
        "{err}"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn a_default_line_with_no_version_exits_127() {
    let world = World::new();
    world.install_client("mysql", "8.4.13", "mysql");
    world.write_config("global.toml", "[mysql]\ndefault = \"9.1\"\n");
    let output = world.run("mysql", &[]);
    assert_eq!(output.status.code(), Some(127));
    let err = stderr(&output);
    assert!(
        err.starts_with("marsa: MySQL 9.1 is not installed (asked for by [mysql] default"),
        "{err}"
    );
    assert!(err.contains("[shim_server_line_not_installed]\n"), "{err}");
}

/// A version folder without the client is a broken install.
#[test]
fn a_missing_client_program_exits_127() {
    let world = World::new();
    world.install_client("postgres", "18.7.0", "psql");
    let output = world.run("pg_dump", &[]);
    assert_eq!(output.status.code(), Some(127));
    let err = stderr(&output);
    assert!(
        err.contains("pg_dump is missing [shim_program_missing]"),
        "{err}"
    );
}

/// `marsad` tells an older copy of the shim from another program by this
/// mark, so the built shim must hold it.
#[test]
fn the_shim_holds_its_mark() {
    let bytes = fs::read(env!("CARGO_BIN_EXE_marsa-shim")).expect("read the shim");
    let mark = SHIM_MARK.as_bytes();
    assert!(bytes.windows(mark.len()).any(|window| window == mark));
    let output = World::new().run("marsa-shim-x", &[]);
    assert!(stderr(&output).contains(SHIM_MARK), "{}", stderr(&output));
}
