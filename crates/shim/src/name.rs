//! Which tool the shim is, from the name it was started as.

use std::ffi::OsStr;
use std::path::Path;

use marsa_core::resolver::Tool;
use marsa_core::runtimes::Runtime;

/// A name the shim answers to. `npm`, `npx`, and `composer` come in task M1-06c.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimName {
    Php,
    Node,
}

impl ShimName {
    /// Reads `argv[0]`. Only the file name counts, so `/any/path/php` is `php`.
    ///
    /// Not `current_exe()`: on Linux it follows symlinks and gives `marsa-shim`.
    /// The match is exact and case-sensitive.
    pub fn from_argv0(argv0: &OsStr) -> Option<Self> {
        match Path::new(argv0).file_name()?.to_str()? {
            "php" => Some(ShimName::Php),
            "node" => Some(ShimName::Node),
            _ => None,
        }
    }

    /// The tool whose version is picked.
    pub fn tool(self) -> Tool {
        match self {
            ShimName::Php => Tool::Php,
            ShimName::Node => Tool::Node,
        }
    }

    /// The folder under `runtimes/` that holds the real program.
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
    }

    #[test]
    fn unknown_names() {
        for text in ["marsa-shim", "PHP", "", "/", "php8", "php.exe"] {
            assert_eq!(name(text), None, "{text:?}");
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
        assert_eq!(ShimName::Php.tool(), Tool::Php);
        assert_eq!(ShimName::Node.tool(), Tool::Node);
        assert_eq!(ShimName::Php.runtime(), Runtime::Php);
        assert_eq!(ShimName::Node.runtime(), Runtime::Node);
    }
}
