//! Checks one of Marsa's own PHP builds (ADR 0012) for macOS or Linux before
//! it goes into the manifest. Parsing the tool output is pure; running the
//! tools is a thin layer with `Command::new(...).args([...])`, never a shell.
//!
//! The extension list is `recipes/php-extensions.txt`, the same file the
//! build workflow (`.github/workflows/php-build.yml`) reads.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::extract::{self, Layout, Limits};
use marsa_core::recipes::Platform;
use semver::Version;

use super::build::Target;

/// Good PHP 8.4 builds export 1,034 to 1,036 `zend_*` symbols, PHP 8.3
/// builds about 980; static musl and broken builds none (S-03, S-10, M5-08).
pub const MIN_ZEND_SYMBOLS: usize = 900;

/// Linux builds target glibc 2.17 (ADR 0012).
pub const MAX_GLIBC: (u32, u32) = (2, 17);

/// The shared extension list, built into xtask.
const EXTENSIONS_FILE: &str = include_str!("../../../recipes/php-extensions.txt");

/// How an extension is built (the first column of `php-extensions.txt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Always built in; spc has no name for it.
    Builtin,
    /// Built into `php` and `php-fpm`.
    Static,
    /// A `.so` in `ext/`, loaded with `zend_extension=`.
    Shared,
}

/// One line of `php-extensions.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub kind: Kind,
    /// The spc name; `None` for [`Kind::Builtin`].
    pub spc: Option<String>,
    /// What `php -m` lists, lowercase.
    pub module: String,
    /// `builtin@8.5`: the module exists from this PHP version on.
    pub since: Option<(u64, u64)>,
}

impl Extension {
    /// Whether PHP `version` has this module.
    pub fn applies_to(&self, version: &Version) -> bool {
        self.since
            .is_none_or(|since| (version.major, version.minor) >= since)
    }
}

impl Extension {
    /// Where the archive keeps a shared extension: `ext/<spc name>.so`.
    pub fn archive_path(&self, root: &Path) -> Option<PathBuf> {
        let spc = self.spc.as_deref().filter(|_| self.kind == Kind::Shared)?;
        Some(root.join("ext").join(format!("{spc}.so")))
    }
}

/// Parses `php-extensions.txt`: `<kind> <spc name> <module name>` per line,
/// `#` comments, blank lines. Refuses unknown kinds and repeated names.
pub fn parse_extensions(text: &str) -> Result<Vec<Extension>> {
    let mut list = Vec::new();
    let (mut spc_seen, mut module_seen) = (BTreeSet::new(), BTreeSet::new());
    for (number, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = || format!("line {}: {line:?}", number + 1);
        let mut words = line.split_whitespace();
        let (Some(kind), Some(spc)) = (words.next(), words.next()) else {
            bail!("{}: needs <kind> <spc name> <module name>", at());
        };
        let module = words.collect::<Vec<_>>().join(" ").to_lowercase();
        ensure!(!module.is_empty(), "{}: no module name", at());
        let (kind, since) = match kind.split_once('@') {
            Some(("builtin", version)) => {
                let since = version
                    .split_once('.')
                    .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)))
                    .with_context(|| format!("{}: builtin@ needs <major>.<minor>", at()))?;
                (Kind::Builtin, Some(since))
            }
            Some(_) => bail!("{}: only builtin takes @<version>", at()),
            None => match kind {
                "builtin" => (Kind::Builtin, None),
                "static" => (Kind::Static, None),
                "shared" => (Kind::Shared, None),
                other => bail!("{}: unknown kind {other:?}", at()),
            },
        };
        let spc = match (kind, spc) {
            (Kind::Builtin, "-") => None,
            (Kind::Builtin, _) => bail!("{}: a builtin module has spc name \"-\"", at()),
            (_, name) => {
                ensure!(
                    !name.is_empty()
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                    "{}: spc name must be [a-z0-9_]+",
                    at()
                );
                ensure!(
                    spc_seen.insert(name.to_owned()),
                    "{}: {name} is listed twice",
                    at()
                );
                Some(name.to_owned())
            }
        };
        ensure!(
            module_seen.insert(module.clone()),
            "{}: module {module} is listed twice",
            at()
        );
        list.push(Extension {
            kind,
            spc,
            module,
            since,
        });
    }
    ensure!(
        list.iter().any(|e| e.kind == Kind::Static),
        "no static extension listed"
    );
    Ok(list)
}

/// The list in `recipes/php-extensions.txt`.
pub fn extensions() -> Result<Vec<Extension>> {
    parse_extensions(EXTENSIONS_FILE).context("recipes/php-extensions.txt")
}

/// The object file format of a PHP build, which decides how `nm` is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    MachO,
    Elf,
}

impl Binary {
    /// macOS builds are Mach-O, Linux builds ELF. `None` for Windows.
    pub fn of(platform: Platform) -> Option<Binary> {
        match platform {
            Platform::DarwinArm64 | Platform::DarwinX64 => Some(Binary::MachO),
            Platform::LinuxX64 | Platform::LinuxArm64 => Some(Binary::Elf),
            Platform::WindowsX64 | Platform::Any => None,
        }
    }

    /// `nm` arguments that list the exported symbols. Apple's `nm` is
    /// llvm-nm, so `-D` also reads a Linux ELF file on a Mac.
    fn nm_exports(self) -> &'static [&'static str] {
        match self {
            Binary::MachO => &["-gU"],
            Binary::Elf => &["-D", "--defined-only"],
        }
    }
}

/// Exported `zend_*` symbols in `nm` output. Mach-O names start with `_`.
pub fn count_zend_symbols(nm_output: &str, binary: Binary) -> usize {
    let prefix = match binary {
        Binary::MachO => "_zend_",
        Binary::Elf => "zend_",
    };
    nm_output
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .filter(|name| name.starts_with(prefix))
        .count()
}

pub fn check_zend_symbols(count: usize) -> Result<()> {
    ensure!(
        count >= MIN_ZEND_SYMBOLS,
        "exports too few zend symbols: {count}, at least {MIN_ZEND_SYMBOLS} needed to load xdebug.so"
    );
    Ok(())
}

/// The newest `GLIBC_x.y` version named in `nm -D --undefined-only` output
/// (`memcpy@GLIBC_2.14`). `None` if no line names one.
pub fn newest_glibc(nm_output: &str) -> Option<(u32, u32)> {
    nm_output
        .split("GLIBC_")
        .skip(1)
        .filter_map(|rest| {
            let version: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let mut parts = version.split('.').map(str::parse::<u32>);
            Some((parts.next()?.ok()?, parts.next()?.ok()?))
        })
        .max()
}

/// `php -n -v` must name this exact version and a static-php-cli build from
/// the pinned line: `Built by static-php-cli 2.8.6` matches `2.8`. spc adds
/// that line to PHP 8.4 and later only, so PHP 8.3 may lack it; the archive's
/// `BUILD-INFO.txt` is checked for every version ([`check_build_info`]).
pub fn check_version_output(output: &str, version: &Version, spc_line: &str) -> Result<()> {
    ensure!(
        output.contains(&format!("PHP {version} ")),
        "php -v does not say PHP {version}"
    );
    let built = output
        .split("Built by static-php-cli ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .map(|word| word.trim_end_matches('.'));
    match built {
        Some(built) => on_spc_line(built, spc_line),
        None if (version.major, version.minor) < (8, 4) => Ok(()),
        None => bail!("php -v has no \"Built by static-php-cli\" line"),
    }
}

/// `BUILD-INFO.txt` (written by tools/php-build/package.sh) must have a
/// `static-php-cli <version>` line on the pinned line.
pub fn check_build_info(text: &str, spc_line: &str) -> Result<()> {
    let built = text
        .lines()
        .find_map(|line| line.trim_end_matches('\r').strip_prefix("static-php-cli "))
        .context("BUILD-INFO.txt has no \"static-php-cli\" line")?;
    on_spc_line(built.trim(), spc_line)
}

fn on_spc_line(built: &str, spc_line: &str) -> Result<()> {
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

/// `php -n -m` against the list: the builtin and static modules that are
/// missing, and the modules that are not in the list. `cgi-fcgi`, which
/// `php-fpm -m` adds, is not counted.
pub fn compare_modules(
    list: &[Extension],
    version: &Version,
    output: &str,
) -> (Vec<String>, Vec<String>) {
    let expected: BTreeSet<&str> = list
        .iter()
        .filter(|e| e.kind != Kind::Shared && e.applies_to(version))
        .map(|e| e.module.as_str())
        .collect();
    let mut have = modules(output, "PHP Modules");
    have.extend(modules(output, "Zend Modules"));
    let have: BTreeSet<&str> = have
        .iter()
        .map(String::as_str)
        .filter(|m| *m != "cgi-fcgi")
        .collect();
    let missing = expected.difference(&have).map(|m| m.to_string()).collect();
    let extra = have.difference(&expected).map(|m| m.to_string()).collect();
    (missing, extra)
}

/// Unpacks one PHP archive into `work` (deleted first) with the recipe's
/// layout, and checks it. Returns warnings to print; an error stops the build.
///
/// Always: every recipe program is there, every shared extension is in
/// `ext/`, each program exports enough Zend symbols, and a Linux build needs
/// no glibc newer than [`MAX_GLIBC`]. Only when this machine can run the
/// build: `php -v`, `php -m` against the list, and loading every shared
/// extension, for `php` and for `php-fpm`.
pub fn check_archive(
    archive: &Path,
    target: &Target<'_>,
    spc_line: &str,
    work: &Path,
) -> Result<Vec<String>> {
    let (recipe, platform, version) = (target.recipe, target.platform, target.version);
    let binary =
        Binary::of(platform).with_context(|| format!("{platform} has no static-php-cli build"))?;
    let list = extensions()?;
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

    let info = work.join("BUILD-INFO.txt");
    let info = fs::read_to_string(&info)
        .with_context(|| format!("{} has no BUILD-INFO.txt", archive.display()))?;
    check_build_info(&info, spc_line).with_context(|| archive.display().to_string())?;

    let mut programs = Vec::new();
    for name in recipe.program_names() {
        let path = recipe
            .program_path(work, name)
            .context("program_names lists only known programs")?;
        ensure!(
            path.is_file(),
            "{} has no {name} where the recipe expects it",
            archive.display()
        );
        programs.push((name, path));
    }
    let mut shared = Vec::new();
    for ext in &list {
        if let Some(path) = ext.archive_path(work) {
            ensure!(
                path.is_file(),
                "{} has no {}",
                archive.display(),
                path.strip_prefix(work).unwrap_or(&path).display()
            );
            shared.push((ext, path));
        }
    }

    let can_run = Platform::current() == Some(platform);
    let mut warnings = Vec::new();
    if !can_run {
        warnings.push(format!(
            "php {version} ({platform}): this machine cannot run it, so php -v, php -m and the extension load tests were skipped"
        ));
    }
    for (name, program) in &programs {
        let what = format!("{name} {version} ({platform})");
        let mut args = binary.nm_exports().to_vec();
        args.push(path_str(program)?);
        let symbols = count_zend_symbols(&run("nm", &args)?, binary);
        check_zend_symbols(symbols).with_context(|| what.clone())?;
        println!("  {what}: {symbols} zend symbols exported");
        if binary == Binary::Elf {
            check_glibc(program, &what, &mut warnings)?;
        }
        if !can_run {
            continue;
        }
        check_version_output(&run(program, &["-n", "-v"])?, version, spc_line)
            .with_context(|| what.clone())?;
        let (missing, extra) = compare_modules(&list, version, &run(program, &["-n", "-m"])?);
        ensure!(
            missing.is_empty(),
            "{what} lacks modules from recipes/php-extensions.txt: {}",
            missing.join(", ")
        );
        if !extra.is_empty() {
            warnings.push(format!(
                "{what} has modules that recipes/php-extensions.txt does not list: {}",
                extra.join(", ")
            ));
        }
        for (ext, so) in &shared {
            let arg = format!("zend_extension={}", path_str(so)?);
            let output = run(program, &["-n", "-d", &arg, "-m"])?;
            ensure!(
                modules(&output, "Zend Modules").contains(&ext.module),
                "{what} did not load {}",
                so.display()
            );
            println!("    {name}: loads {}", so.display());
        }
    }
    for (_, so) in &shared {
        if binary == Binary::Elf {
            check_glibc(so, &format!("{} ({platform})", so.display()), &mut warnings)?;
        }
    }
    Ok(warnings)
}

/// A Linux file must not need a glibc newer than [`MAX_GLIBC`].
fn check_glibc(file: &Path, what: &str, warnings: &mut Vec<String>) -> Result<()> {
    let output = run("nm", &["-D", "--undefined-only", path_str(file)?])?;
    match newest_glibc(&output) {
        Some(newest) => ensure!(
            newest <= MAX_GLIBC,
            "{what} needs glibc {}.{}, newer than {}.{}",
            newest.0,
            newest.1,
            MAX_GLIBC.0,
            MAX_GLIBC.1
        ),
        None => warnings.push(format!(
            "{what}: nm shows no GLIBC_ versions, so the glibc check was skipped"
        )),
    }
    Ok(())
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("{} is not UTF-8", path.display()))
}

/// Runs a program with arguments (no shell) and returns stdout. A non-zero
/// exit is an error that shows stderr.
fn run(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<String> {
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

    // Real output of the S-10 build (PHP 8.4.27, macOS arm64, run 3).
    const PHP_M: &str = include_str!("fixtures/php-m.txt");
    const PHP_FPM_M: &str = include_str!("fixtures/php-fpm-m.txt");
    const PHP_XDEBUG_M: &str = include_str!("fixtures/php-xdebug-m.txt");
    const PHP_V: &str = include_str!("fixtures/php-v.txt");

    // Cut from `nm` on the same builds.
    const NM_MACHO: &str = "0000000103e767c8 S _zend_accel_schedule_restart_hook\n\
        0000000100586e48 T _zend_activate\n\
        0000000100001000 T _php_zend_x\n\
        0000000100002000 T _main\n";
    const NM_ELF: &str = "00000000042545df T ACCESS_DESCRIPTION_free\n\
        0000000000a1b2c3 T zend_activate\n\
        0000000000a1b2c4 T zend_bailout\n\
        0000000000a1b2c5 T php_zend_x\n";
    const NM_ELF_UNDEFINED: &str = "                 w ZSTD_trace_compress_begin\n\
        \x20                U __cmsg_nxthdr@GLIBC_2.2.5\n\
        \x20                U clock_gettime@GLIBC_2.17\n\
        \x20                U memcpy@GLIBC_2.14\n";

    fn list() -> Vec<Extension> {
        extensions().expect("recipes/php-extensions.txt is valid")
    }

    #[test]
    fn repository_extension_list() {
        let list = list();
        let count = |kind| list.iter().filter(|e| e.kind == kind).count();
        assert_eq!(
            (
                count(Kind::Builtin),
                count(Kind::Static),
                count(Kind::Shared)
            ),
            (12, 37, 1)
        );
        // lexbor and uri are on from PHP 8.5 only.
        let new_in_85: Vec<_> = list
            .iter()
            .filter(|e| e.since == Some((8, 5)))
            .map(|e| e.module.as_str())
            .collect();
        assert_eq!(new_in_85, ["lexbor", "uri"]);
        let opcache = list
            .iter()
            .find(|e| e.spc.as_deref() == Some("opcache"))
            .expect("opcache is listed");
        assert_eq!(opcache.module, "zend opcache");
        let xdebug = list
            .iter()
            .find(|e| e.kind == Kind::Shared)
            .expect("xdebug");
        assert_eq!(xdebug.module, "xdebug");
        let root = Path::new("v");
        assert_eq!(
            xdebug.archive_path(root),
            Some(root.join("ext").join("xdebug.so"))
        );
        assert_eq!(opcache.archive_path(root), None);
    }

    #[test]
    fn extension_list_refusals() {
        let good = "builtin - Core\nbuiltin@8.5 - uri\nstatic pdo PDO\r\nshared xdebug Xdebug\n";
        let parsed = parse_extensions(good).expect("valid");
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[1].since, Some((8, 5)));
        assert!(parsed[1].applies_to(&Version::new(8, 5, 0)));
        assert!(parsed[1].applies_to(&Version::new(9, 0, 0)));
        assert!(!parsed[1].applies_to(&Version::new(8, 4, 27)));
        assert!(parsed[0].applies_to(&Version::new(8, 3, 36)));
        for (text, expected) in [
            ("static\n", "needs <kind>"),
            ("static pdo\n", "no module name"),
            ("dynamic pdo PDO\n", "unknown kind"),
            ("builtin core Core\nstatic pdo PDO\n", "spc name \"-\""),
            ("static PDO PDO\n", "[a-z0-9_]+"),
            ("static - PDO\n", "[a-z0-9_]+"),
            ("static pdo PDO\nstatic pdo pdo2\n", "pdo is listed twice"),
            (
                "static pdo PDO\nstatic pdo2 pdo\n",
                "module pdo is listed twice",
            ),
            ("builtin - Core\n", "no static extension"),
            ("builtin@8 - uri\nstatic pdo PDO\n", "builtin@ needs"),
            ("builtin@x.y - uri\nstatic pdo PDO\n", "builtin@ needs"),
            ("static@8.5 pdo PDO\n", "only builtin takes"),
        ] {
            let err = parse_extensions(text).expect_err(expected);
            assert!(format!("{err:#}").contains(expected), "{expected}: {err:#}");
        }
    }

    #[test]
    fn real_php_m_matches_the_list() {
        let list = list();
        let v84 = Version::new(8, 4, 27);
        let (missing, extra) = compare_modules(&list, &v84, PHP_M);
        assert!(missing.is_empty(), "{missing:?}");
        assert!(extra.is_empty(), "{extra:?}");
        // php-fpm adds cgi-fcgi, which does not count.
        assert_eq!(compare_modules(&list, &v84, PHP_FPM_M), (vec![], vec![]));
        // PHP 8.5 must also have lexbor and uri.
        let v85 = Version::new(8, 5, 12);
        let (missing, _) = compare_modules(&list, &v85, PHP_M);
        assert_eq!(missing, ["lexbor", "uri"]);
        let with_85 = PHP_M.replace("libxml\n", "lexbor\nlibxml\nuri\n");
        assert_eq!(compare_modules(&list, &v85, &with_85), (vec![], vec![]));
        // With Xdebug loaded, it is the only extra module.
        let (missing, extra) = compare_modules(&list, &v84, PHP_XDEBUG_M);
        assert!(missing.is_empty());
        assert_eq!(extra, ["xdebug"]);
        assert!(modules(PHP_XDEBUG_M, "Zend Modules").contains(&"xdebug".to_owned()));
    }

    #[test]
    fn missing_and_extra_modules() {
        let output = PHP_M
            .replace("pdo_sqlite\n", "")
            .replace("zip\n", "zip\nyaml\n");
        let v = Version::new(8, 4, 27);
        let (missing, extra) = compare_modules(&list(), &v, &output);
        assert_eq!(missing, ["pdo_sqlite"]);
        assert_eq!(extra, ["yaml"]);
        let output = "[PHP Modules]\nbcmath\nCore\n\n[Zend Modules]\nZend OPcache\n";
        let (missing, _) = compare_modules(&list(), &v, output);
        assert!(missing.contains(&"intl".to_owned()));
        assert!(!missing.contains(&"zend opcache".to_owned()));
        assert!(!missing.contains(&"core".to_owned()));
    }

    #[test]
    fn zend_symbols_per_format() {
        assert_eq!(count_zend_symbols(NM_MACHO, Binary::MachO), 2);
        assert_eq!(count_zend_symbols(NM_ELF, Binary::Elf), 2);
        // The other format's prefix does not count.
        assert_eq!(count_zend_symbols(NM_ELF, Binary::MachO), 0);
        assert_eq!(count_zend_symbols(NM_MACHO, Binary::Elf), 0);
        let many: String = (0..1035)
            .map(|i| format!("{i:016x} T zend_fn_{i}\n"))
            .collect();
        let count = count_zend_symbols(&many, Binary::Elf);
        assert_eq!(count, 1035);
        check_zend_symbols(count).expect("1,035 is enough");
        let err = check_zend_symbols(0).expect_err("0 is too few");
        assert!(err.to_string().contains("exports too few zend symbols"));
    }

    #[test]
    fn binary_format_per_platform() {
        assert_eq!(Binary::of(Platform::DarwinX64), Some(Binary::MachO));
        assert_eq!(Binary::of(Platform::LinuxArm64), Some(Binary::Elf));
        assert_eq!(Binary::of(Platform::WindowsX64), None);
        assert_eq!(Binary::Elf.nm_exports(), ["-D", "--defined-only"]);
    }

    #[test]
    fn newest_glibc_version() {
        assert_eq!(newest_glibc(NM_ELF_UNDEFINED), Some((2, 17)));
        assert!(newest_glibc(NM_ELF_UNDEFINED).expect("found") <= MAX_GLIBC);
        let newer = format!("{NM_ELF_UNDEFINED}  U fstat@GLIBC_2.33\n");
        assert_eq!(newest_glibc(&newer), Some((2, 33)));
        assert!(newest_glibc(&newer).expect("found") > MAX_GLIBC);
        assert_eq!(newest_glibc("  U memcpy\n  U GLIBC_x\n"), None);
    }

    #[test]
    fn spc_line_must_match() {
        let v = Version::new(8, 4, 27);
        check_version_output(PHP_V, &v, "2.8").expect("2.8.5 is on the 2.8 line");
        let crlf = PHP_V.replace('\n', "\r\n");
        check_version_output(&crlf, &v, "2.8").expect("CRLF is fine");
        for (output, version, line, expected) in [
            (
                PHP_V.replace("2.8.5", "3.0.0-alpha1"),
                v.clone(),
                "2.8",
                "pins the 2.8 line",
            ),
            (PHP_V.to_owned(), v.clone(), "2.80", "pins the 2.80 line"),
            (PHP_V.to_owned(), Version::new(8, 4, 2), "2.8", "PHP 8.4.2"),
            (
                PHP_V.replace("Built by static-php-cli 2.8.5\n", ""),
                v.clone(),
                "2.8",
                "no \"Built by",
            ),
        ] {
            let err = check_version_output(&output, &version, line).expect_err(expected);
            assert!(err.to_string().contains(expected), "{err}");
        }
        // PHP 8.3 has no "Built by" line; BUILD-INFO.txt carries it instead.
        let php83 = "PHP 8.3.36 (cli) (built: Nov 11 2026 00:03:02) (NTS clang 17.0.0)\n";
        check_version_output(php83, &Version::new(8, 3, 36), "2.8").expect("8.3 has no line");
        // But if the line is there, it must be on the pinned line.
        let wrong = format!("{php83}Built by static-php-cli 3.0.0\n");
        assert!(check_version_output(&wrong, &Version::new(8, 3, 36), "2.8").is_err());
    }

    #[test]
    fn build_info_names_the_spc_line() {
        let info = "php 8.3.36 for darwin-arm64\r\nstatic-php-cli 2.8.5\r\nxdebug 3.5.3\n";
        check_build_info(info, "2.8").expect("2.8.5 is on the 2.8 line");
        let err = check_build_info(info, "3.0").expect_err("wrong line");
        assert!(err.to_string().contains("pins the 3.0 line"), "{err}");
        let err = check_build_info("php 8.3.36\n", "2.8").expect_err("no line");
        assert!(
            err.to_string().contains("no \"static-php-cli\" line"),
            "{err}"
        );
    }
}
