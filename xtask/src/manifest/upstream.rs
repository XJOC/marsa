//! The sha256 that the upstream project publishes for a file, to compare with
//! the one we compute. Pure functions: the build step downloads the text.

use anyhow::{Context, Result, bail};

/// Where the upstream sha256 of the file at `url` is published, and the name
/// the file has in it. `None` when the source publishes none: static-php-cli
/// (checked by `php_smoke` instead) and `windows.php.net/.../archives/`
/// (it only has `sha1sum.txt`).
pub fn sum_file_for(url: &str) -> Option<(String, String)> {
    let (dir, name) = url.rsplit_once('/')?;
    let sums = if url.starts_with("https://nodejs.org/dist/") {
        format!("{dir}/SHASUMS256.txt")
    } else if dir == "https://windows.php.net/downloads/releases" {
        format!("{dir}/sha256sum.txt")
    } else if url.starts_with("https://getcomposer.org/download/") {
        format!("{url}.sha256sum")
    } else {
        return None;
    };
    Some((sums, name.to_owned()))
}

/// Finds `name` in a sum file: one `<hex> <name>` per line, with one or two
/// spaces, and `*` before the name for binary mode (`sha256sum.txt` on
/// windows.php.net). `\r\n` endings are fine.
pub fn find_sha256(text: &str, name: &str) -> Result<[u8; 32]> {
    for line in text.lines().map(|l| l.trim_end_matches('\r')) {
        let Some((hex, file)) = line.split_once(' ') else {
            continue;
        };
        let file = file.trim_start_matches(' ');
        let file = file.strip_prefix('*').unwrap_or(file);
        if file == name {
            return parse_hex(&hex.to_ascii_lowercase())
                .with_context(|| format!("bad sha256 for {name}: {hex:?}"));
        }
    }
    bail!("{name} is not in the upstream sum file")
}

/// 64 lowercase hex characters to bytes.
pub fn parse_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Bytes to 64 lowercase hex characters.
pub fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cut from the real files (2026-11-04), with Windows line endings.
    const NODE: &str = "c219410bea0aed08ecfa5847a3d8aca4e45dd494a094f86c0d8721138360a9c9  node-v22.23.4-aix-ppc64.tar.gz\r\n\
        c9e1dbc748b33ed2d14f42445191ecaf14c56259c7a9a26d0042c49ec2b66428  node-v22.23.4-darwin-arm64.tar.gz\r\n";
    const PHP_WINDOWS: &str = "cdbb85b45f38f282f05764ca08648b5f92db99c75b2fb3848eb4a559f6553b48 *php-7.4.33-Win32-vc15-x64.zip\r\n\
        dbae154a0a89791f36c3d48e68bae797878333e7d6cb5721e74617de59b63ab7 *php-8.4.27-1-nts-Win32-vs17-x64.zip\r\n";
    const COMPOSER: &str =
        "6f1b891e1d4927d6274027dc540fb490e47689e19e102c49e8da849794882f8c  composer.phar\r\n";

    #[test]
    fn reads_the_three_formats() {
        let node = find_sha256(NODE, "node-v22.23.4-darwin-arm64.tar.gz").expect("node");
        assert_eq!(
            hex(&node),
            "c9e1dbc748b33ed2d14f42445191ecaf14c56259c7a9a26d0042c49ec2b66428"
        );
        let php = find_sha256(PHP_WINDOWS, "php-8.4.27-1-nts-Win32-vs17-x64.zip").expect("php");
        assert_eq!(
            hex(&php),
            "dbae154a0a89791f36c3d48e68bae797878333e7d6cb5721e74617de59b63ab7"
        );
        let composer = find_sha256(COMPOSER, "composer.phar").expect("composer");
        assert_eq!(
            hex(&composer),
            "6f1b891e1d4927d6274027dc540fb490e47689e19e102c49e8da849794882f8c"
        );
    }

    #[test]
    fn missing_name_is_an_error() {
        for (text, name) in [
            (NODE, "node-v22.23.4-linux-x64.tar.gz"),
            (PHP_WINDOWS, "php-8.4.27-nts-Win32-vs17-x64.zip"),
            (COMPOSER, "composer.phar.sig"),
            ("", "composer.phar"),
        ] {
            let err = find_sha256(text, name).expect_err(name);
            assert!(err.to_string().contains("is not in"), "{err}");
        }
    }

    #[test]
    fn sum_file_urls() {
        let cases = [
            (
                "https://nodejs.org/dist/v22.23.4/node-v22.23.4-win-x64.zip",
                Some((
                    "https://nodejs.org/dist/v22.23.4/SHASUMS256.txt",
                    "node-v22.23.4-win-x64.zip",
                )),
            ),
            (
                "https://windows.php.net/downloads/releases/php-8.4.27-1-nts-Win32-vs17-x64.zip",
                Some((
                    "https://windows.php.net/downloads/releases/sha256sum.txt",
                    "php-8.4.27-1-nts-Win32-vs17-x64.zip",
                )),
            ),
            (
                "https://getcomposer.org/download/2.11.1/composer.phar",
                Some((
                    "https://getcomposer.org/download/2.11.1/composer.phar.sha256sum",
                    "composer.phar",
                )),
            ),
            (
                "https://windows.php.net/downloads/releases/archives/php-8.4.23-nts-Win32-vs17-x64.zip",
                None,
            ),
            (
                "https://dl.static-php.dev/static-php-cli/bulk/php-8.4.23-cli-macos-aarch64.tar.gz",
                None,
            ),
        ];
        for (url, expected) in cases {
            let got = sum_file_for(url);
            let expected = expected.map(|(a, b)| (a.to_owned(), b.to_owned()));
            assert_eq!(got, expected, "{url}");
        }
    }

    #[test]
    fn hex_round_trip_and_rules() {
        let text = "2e17f6527a6c92f5713ba16e5dfd1b55869ab5e98de46677e994fed5536316ba";
        assert_eq!(hex(&parse_hex(text).expect("valid")), text);
        assert!(parse_hex(&text.to_uppercase()).is_none());
        assert!(parse_hex(&text[..63]).is_none());
    }
}
