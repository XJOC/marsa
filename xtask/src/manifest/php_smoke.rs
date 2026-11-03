//! Checks a static-php-cli PHP build for macOS before it goes into the
//! manifest (ADR 0010). Parsing the tool output is pure; running the tools is
//! a thin layer with `Command::new(...).args([...])`, never a shell.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::extract::{self, Layout, Limits};
use marsa_core::recipes::Platform;
use semver::Version;

use super::build::Target;

/// Good builds export about 1,011 `zend_*` symbols; broken ones none (S-03).
pub const MIN_ZEND_SYMBOLS: usize = 1000;

/// The extensions a Laravel app needs (S-03, question 4), as `php -m` lists
/// them (lowercase). OPcache shows as `Zend OPcache` under `[Zend Modules]`.
const LARAVEL_EXTENSIONS: [&str; 33] = [
    "bcmath",
    "ctype",
    "curl",
    "dom",
    "exif",
    "fileinfo",
    "filter",
    "gd",
    "iconv",
    "intl",
    "mbstring",
    "mysqlnd",
    "zend opcache",
    "openssl",
    "pcntl",
    "pdo",
    "pdo_mysql",
    "pdo_pgsql",
    "pdo_sqlite",
    "phar",
    "posix",
    "redis",
    "session",
    "simplexml",
    "sockets",
    "sodium",
    "sqlite3",
    "tokenizer",
    "xml",
    "xmlreader",
    "xmlwriter",
    "zip",
    "zlib",
];

/// Lines of `nm -gU` output that name a global `_zend_` symbol.
pub fn count_zend_symbols(nm_output: &str) -> usize {
    nm_output
        .lines()
        .filter(|line| line.contains(" _zend_"))
        .count()
}

pub fn check_zend_symbols(count: usize) -> Result<()> {
    ensure!(
        count >= MIN_ZEND_SYMBOLS,
        "exports too few zend symbols: {count}, at least {MIN_ZEND_SYMBOLS} needed to load xdebug.so"
    );
    Ok(())
}

/// `php -n -v` must name this exact version and a static-php-cli build from
/// the pinned line: `Built by static-php-cli 2.8.6` matches `2.8`.
pub fn check_version_output(output: &str, version: &Version, spc_line: &str) -> Result<()> {
    ensure!(
        output.contains(&format!("PHP {version} ")),
        "php -v does not say PHP {version}"
    );
    let built = output
        .split("Built by static-php-cli ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .map(|word| word.trim_end_matches('.'))
        .context("php -v has no \"Built by static-php-cli\" line")?;
    ensure!(
        built == spc_line || built.starts_with(&format!("{spc_line}.")),
        "built by static-php-cli {built}, but pins.toml pins the {spc_line} line"
    );
    Ok(())
}

/// The names listed under `[<section>]` in `php -m` output, lowercase.
pub fn modules(output: &str, section: &str) -> Vec<String> {
    let header = format!("[{section}]");
    output
        .lines()
        .map(|l| l.trim_end_matches('\r').trim())
        .skip_while(|l| *l != header)
        .skip(1)
        .take_while(|l| !l.starts_with('['))
        .filter(|l| !l.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The Laravel extensions that `php -n -m` does not list.
pub fn missing_laravel_extensions(output: &str) -> Vec<&'static str> {
    let mut have = modules(output, "PHP Modules");
    have.extend(modules(output, "Zend Modules"));
    LARAVEL_EXTENSIONS
        .into_iter()
        .filter(|ext| !have.iter().any(|h| h == ext))
        .collect()
}

/// Unpacks one PHP archive into `work` (deleted first) and checks each program
/// in it. Returns warnings to print; an error stops the build.
///
/// `xdebug` is an absolute path to an `xdebug.so` for this PHP minor.
pub fn check_archive(
    archive: &Path,
    target: &Target<'_>,
    spc_line: &str,
    xdebug: Option<&Path>,
    work: &Path,
) -> Result<Vec<String>> {
    let (recipe, platform, version) = (target.recipe, target.platform, target.version);
    if work.exists() {
        fs::remove_dir_all(work).with_context(|| format!("cannot clear {}", work.display()))?;
    }
    fs::create_dir_all(work).with_context(|| format!("cannot create {}", work.display()))?;
    extract::extract(
        archive,
        &Layout::from(target.artifact),
        work,
        &Limits::default(),
    )
    .with_context(|| format!("cannot unpack {}", archive.display()))?;

    let programs: Vec<(String, PathBuf)> = recipe
        .program_names()
        .filter_map(|name| Some((name.to_owned(), recipe.program_path(work, name)?)))
        .filter(|(_, path)| path.is_file())
        .collect();
    ensure!(
        !programs.is_empty(),
        "{} holds none of the recipe's programs",
        archive.display()
    );

    // Only an arm64 Mac can run the darwin-arm64 build.
    let can_run = platform == Platform::DarwinArm64
        && std::env::consts::OS == "macos"
        && std::env::consts::ARCH == "aarch64";
    let mut warnings = Vec::new();
    for (name, program) in &programs {
        let symbols = count_zend_symbols(&run("nm", &["-gU".as_ref(), program.as_os_str()])?);
        check_zend_symbols(symbols).with_context(|| format!("{name} ({platform})"))?;
        println!("  {name} {version} ({platform}): {symbols} zend symbols exported");
        if !can_run {
            if platform == Platform::DarwinArm64 {
                warnings.push(format!(
                    "{name} {version} ({platform}): not an arm64 Mac, so php -v, php -m and the xdebug test were skipped"
                ));
            }
            continue;
        }
        check_version_output(&run_os(program, &["-n", "-v"])?, version, spc_line)
            .with_context(|| format!("{name} ({platform})"))?;
        if name != "php" {
            continue;
        }
        let missing = missing_laravel_extensions(&run_os(program, &["-n", "-m"])?);
        if !missing.is_empty() {
            warnings.push(format!(
                "php {version} ({platform}) lacks Laravel extensions: {}",
                missing.join(", ")
            ));
        }
        match xdebug {
            Some(so) => {
                let arg = format!("zend_extension={}", so.display());
                let output = run_os(program, &["-n", "-d", &arg, "-m"])?;
                ensure!(
                    modules(&output, "Zend Modules").iter().any(|m| m == "xdebug"),
                    "php {version} ({platform}) did not load {}",
                    so.display()
                );
                println!("    php: loads {}", so.display());
            }
            None => warnings.push(format!(
                "php {version} ({platform}): no --xdebug-so for {}.{}, the zend_extension load test was skipped",
                version.major, version.minor
            )),
        }
    }
    Ok(warnings)
}

fn run_os(program: &Path, args: &[&str]) -> Result<String> {
    let args: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_ref()).collect();
    run(program.as_os_str(), &args)
}

/// Runs a program with arguments (no shell) and returns stdout. A non-zero
/// exit is an error that shows stderr.
fn run(program: impl AsRef<std::ffi::OsStr>, args: &[&std::ffi::OsStr]) -> Result<String> {
    let program = program.as_ref();
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("cannot run {}", program.to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "{} failed ({}): {}",
            program.to_string_lossy(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nm_output(zend: usize) -> String {
        let mut text = String::from("0000000100001000 T _main\n0000000100002000 T _php_info\n");
        for i in 0..zend {
            text.push_str(&format!("00000001000{i:05x} T _zend_fn_{i}\n"));
        }
        text
    }

    #[test]
    fn zend_symbol_count() {
        let good = count_zend_symbols(&nm_output(1011));
        assert_eq!(good, 1011);
        check_zend_symbols(good).expect("1,011 is enough");
        let broken = count_zend_symbols(&nm_output(0));
        assert_eq!(broken, 0);
        let err = check_zend_symbols(broken).expect_err("0 is too few");
        assert!(err.to_string().contains("exports too few zend symbols"));
        // `_php_zend_x` is not a `_zend_` symbol.
        assert_eq!(count_zend_symbols("1 T _php_zend_x\n"), 0);
    }

    const V_OUTPUT: &str = "PHP 8.4.23 (cli) (built: Jul  7 2026 06:01:12) (NTS)\r\n\
        Copyright (c) The PHP Group\r\n\
        Built by static-php-cli 2.8.6\r\n\
        Zend Engine v4.4.23, Copyright (c) Zend Technologies\r\n";

    #[test]
    fn spc_line_must_match() {
        let v = Version::new(8, 4, 23);
        check_version_output(V_OUTPUT, &v, "2.8").expect("2.8.6 is on the 2.8 line");
        for (output, version, line, expected) in [
            (
                V_OUTPUT.replace("2.8.6", "3.0.0-alpha1"),
                v.clone(),
                "2.8",
                "pins the 2.8 line",
            ),
            (V_OUTPUT.to_owned(), v.clone(), "2.80", "pins the 2.80 line"),
            (
                V_OUTPUT.to_owned(),
                Version::new(8, 4, 2),
                "2.8",
                "PHP 8.4.2",
            ),
            (
                V_OUTPUT.replace("Built by static-php-cli 2.8.6\r\n", ""),
                v.clone(),
                "2.8",
                "no \"Built by",
            ),
        ] {
            let err = check_version_output(&output, &version, line).expect_err(expected);
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    #[test]
    fn module_sections_and_missing_extensions() {
        let output = "[PHP Modules]\nbcmath\nCore\nPDO\npdo_mysql\nSimpleXML\n\n[Zend Modules]\nXdebug\nZend OPcache\n\n";
        assert_eq!(modules(output, "Zend Modules"), ["xdebug", "zend opcache"]);
        let missing = missing_laravel_extensions(output);
        assert!(missing.contains(&"pdo_sqlite"));
        assert!(missing.contains(&"intl"));
        for present in ["bcmath", "pdo", "pdo_mysql", "simplexml", "zend opcache"] {
            assert!(!missing.contains(&present), "{present}");
        }
    }
}
