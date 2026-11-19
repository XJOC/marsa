//! Which tool the shim is, from the name it was started as.

use std::ffi::OsStr;
use std::path::Path;

use marsa_core::resolver::Tool;
use marsa_core::runtimes::Runtime;

/// A name the shim answers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimName {
    Php,
    Node,
    /// Runs through `node`, with npm's own `npm-cli.js`.
    Npm,
    /// Runs through `node`, with npm's own `npx-cli.js`.
    Npx,
    /// Runs through `php`, with `composer.phar`.
    Composer,
}

impl ShimName {
    /// Reads `argv[0]`. Only the file name counts, so `/any/path/php` is `php`.
    ///
    /// Not `current_exe()`: on Linux it follows symlinks and gives `marsa-shim`.
    /// The match is exact and case-sensitive.
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "Windows reads its own program path instead")
    )]
    pub fn from_argv0(argv0: &OsStr) -> Option<Self> {
        Self::from_name(Path::new(argv0).file_name()?.to_str()?)
    }

    /// Reads the path of a Windows program, such as `C:\...\shims\php.exe`
    /// from `current_exe()`, or `php` from `argv[0]`: the file name without
    /// `.exe`, in any letter case, as Windows itself finds `PHP` and
    /// `php.EXE`. Another extension (`npm.cmd`) is not a shim.
    #[cfg_attr(
        all(unix, not(test)),
        expect(dead_code, reason = "only `sys/windows.rs` reads a program path")
    )]
    pub fn from_exe_path(path: &Path) -> Option<Self> {
        let exe = path
            .extension()
            .is_none_or(|extension| extension.eq_ignore_ascii_case("exe"));
        if !exe {
            return None;
        }
        Self::from_name(&path.file_stem()?.to_str()?.to_ascii_lowercase())
    }

    /// The exact name, case-sensitive.
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "php" => Some(ShimName::Php),
            "node" => Some(ShimName::Node),
            "npm" => Some(ShimName::Npm),
            "npx" => Some(ShimName::Npx),
            "composer" => Some(ShimName::Composer),
            _ => None,
        }
    }

    /// The tool whose version is picked.
    pub fn tool(self) -> Tool {
        match self {
            ShimName::Php | ShimName::Composer => Tool::Php,
            ShimName::Node | ShimName::Npm | ShimName::Npx => Tool::Node,
        }
    }

    /// The folder under `runtimes/` that holds the real program: `php` for
    /// `composer`, `node` for `npm` and `npx`.
    pub fn runtime(self) -> Runtime {
        self.tool().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> Option<ShimName> {
        ShimName::from_argv0(OsStr::new(text))
    }

    #[test]
    fn known_names() {
        assert_eq!(name("php"), Some(ShimName::Php));
        assert_eq!(name("/any/path/php"), Some(ShimName::Php));
        assert_eq!(name("node"), Some(ShimName::Node));
        assert_eq!(name("shims/node"), Some(ShimName::Node));
        assert_eq!(name("npm"), Some(ShimName::Npm));
        assert_eq!(name("npx"), Some(ShimName::Npx));
        assert_eq!(name("/any/path/composer"), Some(ShimName::Composer));
    }

    #[test]
    fn unknown_names() {
        for text in [
            "marsa-shim",
            "PHP",
            "",
            "/",
            "php8",
            "php.exe",
            "Composer",
            "npm.cmd",
            // Caddy is a runtime that `marsad` runs, not a shim tool.
            "caddy",
            "caddy.exe",
        ] {
            assert_eq!(name(text), None, "{text:?}");
        }
    }

    fn exe(parts: &[&str]) -> Option<ShimName> {
        let path: std::path::PathBuf = parts.iter().collect();
        ShimName::from_exe_path(&path)
    }

    #[test]
    fn windows_names_ignore_the_letter_case_and_exe() {
        for (parts, expected) in [
            (&["C:", "shims", "php.exe"][..], ShimName::Php),
            (&["shims", "PHP.EXE"], ShimName::Php),
            (&["php"], ShimName::Php),
            (&["Node.Exe"], ShimName::Node),
            (&["npm.exe"], ShimName::Npm),
            (&["NPX"], ShimName::Npx),
            (&["shims", "composer.exe"], ShimName::Composer),
        ] {
            assert_eq!(exe(parts), Some(expected), "{parts:?}");
        }
        for parts in [
            &["marsa-shim.exe"][..],
            &["npm.cmd"],
            &["composer.bat"],
            &["php.exe.old"],
            &["php8.exe"],
            &["caddy.exe"],
            &[""],
        ] {
            assert_eq!(exe(parts), None, "{parts:?}");
        }
    }

    // Only Unix lets a file name hold bytes that are not UTF-8.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_unknown() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(ShimName::from_argv0(OsStr::from_bytes(b"ph\xffp")), None);
    }

    #[test]
    fn tool_and_runtime() {
        for (name, tool, runtime) in [
            (ShimName::Php, Tool::Php, Runtime::Php),
            (ShimName::Composer, Tool::Php, Runtime::Php),
            (ShimName::Node, Tool::Node, Runtime::Node),
            (ShimName::Npm, Tool::Node, Runtime::Node),
            (ShimName::Npx, Tool::Node, Runtime::Node),
        ] {
            assert_eq!(name.tool(), tool, "{name:?}");
            assert_eq!(name.runtime(), runtime, "{name:?}");
        }
    }
}
