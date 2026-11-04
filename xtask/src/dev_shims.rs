//! `cargo xtask dev-shims`: set up the shims by hand, before the installer
//! exists (milestone 2), so `php -v` can be tried in a terminal.
//!
//! The plan (which folders and links to make) is a pure function. Doing it is a
//! thin layer on top, so the plan can be tested without building anything.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use marsa_core::config;

/// The names `marsa-shim` answers to (architecture.md section 7).
const SHIM_NAMES: [&str; 5] = ["php", "node", "npm", "npx", "composer"];
/// One folder per tool under `runtimes/`; versions go inside by hand.
const RUNTIME_DIRS: [&str; 3] = ["php", "node", "composer"];

/// One thing the command does on disk.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Make this folder (and its parents) if it is missing.
    Dir(PathBuf),
    /// Replace whatever is at this path with a link to `marsa-shim`.
    Link(PathBuf),
}

impl Step {
    fn path(&self) -> &Path {
        match self {
            Step::Dir(path) | Step::Link(path) => path,
        }
    }
}

/// The steps for a data folder: `shims/`, the five links, then `runtimes/*`.
fn plan(data_dir: &Path) -> Vec<Step> {
    let shims = data_dir.join("shims");
    let runtimes = data_dir.join("runtimes");
    let mut steps = vec![Step::Dir(shims.clone())];
    steps.extend(SHIM_NAMES.iter().map(|name| Step::Link(shims.join(name))));
    steps.extend(
        RUNTIME_DIRS
            .iter()
            .map(|name| Step::Dir(runtimes.join(name))),
    );
    steps
}

/// Do one step. Returns a short word for the report.
fn apply_step(step: &Step, shim_exe: &Path) -> Result<&'static str> {
    match step {
        Step::Dir(dir) => {
            let existed = dir.is_dir();
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
            Ok(if existed { "exists" } else { "created" })
        }
        Step::Link(link) => {
            match fs::remove_file(link) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                // A folder in the way ends up here too: we never delete folders.
                Err(e) => {
                    return Err(e).with_context(|| format!("cannot replace {}", link.display()));
                }
            }
            if fs::hard_link(shim_exe, link).is_ok() {
                return Ok("hard link");
            }
            // For example the repository and the data folder are on different volumes.
            fs::copy(shim_exe, link)
                .with_context(|| format!("cannot copy to {}", link.display()))?;
            Ok("copy")
        }
    }
}

/// Build `marsa-shim` alone (architecture.md section 3: building it together
/// with other packages would switch on the core's `full` feature) and return
/// the path of the result.
fn build_shim(root: &Path) -> Result<PathBuf> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "-p", "marsa-shim", "--release"])
        .current_dir(root)
        .status()
        .context("cannot start cargo")?;
    if !status.success() {
        bail!("cargo build -p marsa-shim --release failed ({status})");
    }
    let target =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), |dir| root.join(dir));
    let exe = target.join("release").join("marsa-shim");
    if !exe.is_file() {
        bail!(
            "built, but {} is missing (is the target folder moved?)",
            exe.display()
        );
    }
    Ok(exe)
}

pub fn run(data_dir: Option<PathBuf>, dry_run: bool) -> Result<()> {
    // `cfg!` is a plain bool, so this file compiles everywhere with no `#[cfg]`.
    if cfg!(windows) {
        bail!("dev-shims is not supported on Windows yet (M1-07)");
    }
    let custom = data_dir.is_some();
    let data_dir = match data_dir {
        Some(dir) => std::path::absolute(&dir)
            .with_context(|| format!("bad --data-dir {}", dir.display()))?,
        None => config::app_data_dir().context("cannot find Marsa's data folder")?,
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| anyhow!("xtask is not inside the workspace"))?;
    let steps = plan(&data_dir);

    println!("Data folder: {}", data_dir.display());
    if dry_run {
        println!("Dry run, nothing is built or written. Plan:");
        for step in &steps {
            match step {
                Step::Dir(path) => println!("  dir   {}", path.display()),
                Step::Link(path) => println!("  link  {} -> marsa-shim", path.display()),
            }
        }
    } else {
        let shim_exe = build_shim(root)?;
        println!("Shim binary: {}", shim_exe.display());
        for step in &steps {
            let how = apply_step(step, &shim_exe)?;
            println!("  {how:<9} {}", step.path().display());
        }
    }

    // The shim always reads the OS data folder: there is no override yet.
    if custom && config::app_data_dir().ok().as_deref() != Some(data_dir.as_path()) {
        println!("\nNote: marsa-shim reads the default data folder only, so these links");
        println!("will not find runtimes here. A custom --data-dir tests this command itself.");
    }
    println!("\nAdd the shims to your PATH (for example in ~/.zshrc):\n");
    println!(
        "  export PATH=\"{}:$PATH\"",
        data_dir.join("shims").display()
    );
    println!("\nPut PHP, Node, and Composer there by hand. The folder name is the version:\n");
    println!(
        "  {}",
        data_dir.join("runtimes/php/<version>/bin/php").display()
    );
    println!(
        "  {}",
        data_dir.join("runtimes/node/<version>/bin/node").display()
    );
    println!(
        "  {}",
        data_dir
            .join("runtimes/composer/<version>/composer.phar")
            .display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// A stand-in for `target/release/marsa-shim`.
    fn fake_shim(dir: &Path, content: &str) -> PathBuf {
        let path = dir.join("marsa-shim");
        fs::write(&path, content).expect("write fake shim");
        path
    }

    fn apply_all(data_dir: &Path, shim_exe: &Path) -> Result<()> {
        plan(data_dir)
            .iter()
            .try_for_each(|step| apply_step(step, shim_exe).map(drop))
    }

    #[test]
    fn plan_has_shims_dir_then_five_links_then_runtime_dirs() {
        let data = Path::new("data");
        let dir = |path: &str| Step::Dir(data.join(path));
        let link = |path: &str| Step::Link(data.join(path));
        let expected = vec![
            dir("shims"),
            link("shims/php"),
            link("shims/node"),
            link("shims/npm"),
            link("shims/npx"),
            link("shims/composer"),
            dir("runtimes/php"),
            dir("runtimes/node"),
            dir("runtimes/composer"),
        ];
        assert_eq!(plan(data), expected);
    }

    #[test]
    fn creates_missing_folders_and_links_to_the_shim() {
        let tmp = tempdir().expect("tempdir");
        let shim = fake_shim(tmp.path(), "shim v1");
        let data = tmp.path().join("deep/data");

        apply_all(&data, &shim).expect("apply");

        for name in SHIM_NAMES {
            assert_eq!(
                fs::read_to_string(data.join("shims").join(name)).expect("read link"),
                "shim v1"
            );
        }
        for name in RUNTIME_DIRS {
            assert!(data.join("runtimes").join(name).is_dir());
        }
    }

    /// Caddy is installed by `marsa install caddy` and run by `marsad`: no
    /// shim, and no folder made by hand here.
    #[test]
    fn caddy_is_not_a_shim() {
        assert!(!SHIM_NAMES.contains(&"caddy"));
        assert!(!RUNTIME_DIRS.contains(&"caddy"));
    }

    // Inode numbers exist on Unix only; the command itself is Unix-only for now.
    #[cfg(unix)]
    #[test]
    fn links_are_hard_links() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempdir().expect("tempdir");
        let shim = fake_shim(tmp.path(), "shim v1");

        apply_all(tmp.path(), &shim).expect("apply");

        let inode = fs::metadata(&shim).expect("shim metadata").ino();
        let link = fs::metadata(tmp.path().join("shims/php")).expect("link metadata");
        assert_eq!(link.ino(), inode);
    }

    #[test]
    fn replaces_old_files_and_runs_twice() {
        let tmp = tempdir().expect("tempdir");
        let shim = fake_shim(tmp.path(), "shim v2");
        fs::create_dir_all(tmp.path().join("shims")).expect("mkdir");
        fs::write(tmp.path().join("shims/php"), "old").expect("write old");

        apply_all(tmp.path(), &shim).expect("first run");
        apply_all(tmp.path(), &shim).expect("second run");

        assert_eq!(
            fs::read_to_string(tmp.path().join("shims/php")).expect("read"),
            "shim v2"
        );
    }

    #[test]
    fn a_folder_named_like_a_shim_is_an_error_not_deleted() {
        let tmp = tempdir().expect("tempdir");
        let shim = fake_shim(tmp.path(), "shim v1");
        fs::create_dir_all(tmp.path().join("shims/node/keep")).expect("mkdir");

        let err = apply_all(tmp.path(), &shim).expect_err("must refuse");

        assert!(format!("{err:#}").contains("cannot replace"), "{err:#}");
        assert!(tmp.path().join("shims/node/keep").is_dir());
    }
}
