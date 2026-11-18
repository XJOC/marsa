//! The hash that the upstream project publishes for a file, to compare with
//! the one we compute. Most publish sha256; Caddy publishes sha512. The
//! manifest itself always stores sha256. Pure functions: the build step
//! downloads the text.

use anyhow::{Context, Result, bail, ensure};

/// Where our own PHP builds are published: one GitHub Release per build,
/// `php-<version>-r<revision>`, made by `.github/workflows/php-build.yml`
/// with a `sha256sums.txt` beside the archives (ADR 0012).
pub const PHP_RELEASES: &str = "https://github.com/XJOC/marsa/releases/download/php-";

/// Caddy's official releases (ADR 0008): tag `v<version>`, with
/// `caddy_<version>_checksums.txt` (sha512) beside the archives.
pub const CADDY_RELEASES: &str = "https://github.com/caddyserver/caddy/releases/download/v";

/// The PostgreSQL builds of theseus-rs (ADR 0023): tag `<version>`, with
/// `<file>.sha256` beside every archive.
pub const THESEUS_RELEASES: &str =
    "https://github.com/theseus-rs/postgresql-binaries/releases/download/";

/// The hash in a sum file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha256,
    Sha512,
}

impl Algorithm {
    /// `sha256` or `sha512`, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha512 => "sha512",
        }
    }

    /// The length of the hash in bytes.
    fn size(self) -> usize {
        match self {
            Algorithm::Sha256 => 32,
            Algorithm::Sha512 => 64,
        }
    }
}

/// How a sum file lists its hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Lines for many files; [`find_sum`] picks the line of ours.
    List,
    /// One line, for our file only; [`single_sum`] reads it.
    Single,
}

/// Where the upstream hash of one file is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumFile {
    /// The URL of the sum file.
    pub url: String,
    /// The name the file has in it.
    pub name: String,
    pub algorithm: Algorithm,
    pub layout: Layout,
}

impl SumFile {
    /// The hash of [`SumFile::name`] in `text`, the body of [`SumFile::url`].
    pub fn find(&self, text: &str) -> Result<Vec<u8>> {
        match self.layout {
            Layout::List => find_sum(text, &self.name, self.algorithm),
            Layout::Single => single_sum(text, &self.name, self.algorithm),
        }
    }
}

/// Where the upstream hash of the file at `url` is published. `None` when
/// the source publishes none: `windows.php.net/.../archives/` (it only has
/// `sha1sum.txt`) and any other host.
pub fn sum_file_for(url: &str) -> Option<SumFile> {
    let (dir, name) = url.rsplit_once('/')?;
    let sum = |url: String, algorithm| {
        Some(SumFile {
            url,
            name: name.to_owned(),
            algorithm,
            layout: Layout::List,
        })
    };
    if url.starts_with(PHP_RELEASES) {
        sum(format!("{dir}/sha256sums.txt"), Algorithm::Sha256)
    } else if url.starts_with("https://nodejs.org/dist/") {
        sum(format!("{dir}/SHASUMS256.txt"), Algorithm::Sha256)
    } else if dir == "https://windows.php.net/downloads/releases" {
        sum(format!("{dir}/sha256sum.txt"), Algorithm::Sha256)
    } else if url.starts_with("https://getcomposer.org/download/") {
        sum(format!("{url}.sha256sum"), Algorithm::Sha256)
    } else if let Some(version) = dir.strip_prefix(CADDY_RELEASES) {
        // `.../v<version>/<file>`: the tag is the only part between.
        if version.is_empty() || version.contains('/') {
            return None;
        }
        sum(
            format!("{dir}/caddy_{version}_checksums.txt"),
            Algorithm::Sha512,
        )
    } else if let Some(tag) = dir.strip_prefix(THESEUS_RELEASES) {
        if tag.is_empty() || tag.contains('/') || name.is_empty() {
            return None;
        }
        // theseus-rs is a third party, and it serves this `.sha256` from the
        // same place as the archive, so the sum proves only that the download
        // is whole. The trust comes from the owner's signature on the manifest
        // that pins this hash (ADR 0023, section 3). M6-09 adds the check of
        // the GitHub artifact attestations.
        Some(SumFile {
            url: format!("{url}.sha256"),
            name: name.to_owned(),
            algorithm: Algorithm::Sha256,
            layout: Layout::Single,
        })
    } else {
        None
    }
}

/// Reads a sum file for one file only, as `sha256sum` writes it and as
/// theseus-rs publishes it: one line, `<hex>  <name>`, with two spaces, a
/// lowercase hex of the length of `algorithm`, and `name` exactly. The line
/// may end with `\n` or `\r\n`. Anything else is refused: an empty file,
/// a second line (even an empty one), or another name.
pub fn single_sum(text: &str, name: &str, algorithm: Algorithm) -> Result<Vec<u8>> {
    let line = text.strip_suffix('\n').unwrap_or(text);
    let line = line.strip_suffix('\r').unwrap_or(line);
    ensure!(
        !line.is_empty(),
        "the upstream sum file for {name} is empty"
    );
    ensure!(
        !line.contains(['\n', '\r']),
        "the upstream sum file for {name} has more than one line"
    );
    let Some((hex, file)) = line.split_once("  ") else {
        bail!("the upstream sum file for {name} is not `<hex>  <name>`: {line:?}");
    };
    ensure!(
        file == name,
        "the upstream sum file is for {file:?}, not for {name}"
    );
    parse_hex_bytes(hex, algorithm.size())
        .with_context(|| format!("bad {} for {name}: {hex:?}", algorithm.name()))
}

/// Finds `name` in a sum file: one `<hex> <name>` per line, with one or two
/// spaces, and `*` before the name for binary mode (`sha256sum.txt` on
/// windows.php.net). `\r\n` endings are fine. The hex must have the length
/// of `algorithm`.
pub fn find_sum(text: &str, name: &str, algorithm: Algorithm) -> Result<Vec<u8>> {
    for line in text.lines().map(|l| l.trim_end_matches('\r')) {
        let Some((hex, file)) = line.split_once(' ') else {
            continue;
        };
        let file = file.trim_start_matches(' ');
        let file = file.strip_prefix('*').unwrap_or(file);
        if file == name {
            return parse_hex_bytes(&hex.to_ascii_lowercase(), algorithm.size())
                .with_context(|| format!("bad {} for {name}: {hex:?}", algorithm.name()));
        }
    }
    bail!("{name} is not in the upstream sum file")
}

/// 64 lowercase hex characters to bytes.
pub fn parse_hex(text: &str) -> Option<[u8; 32]> {
    parse_hex_bytes(text, 32)?.try_into().ok()
}

/// `2 * len` lowercase hex characters to `len` bytes.
fn parse_hex_bytes(text: &str, len: usize) -> Option<Vec<u8>> {
    if text.len() != len * 2 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    (0..len)
        .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// Bytes to lowercase hex, two characters per byte.
pub fn hex(bytes: &[u8]) -> String {
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
    // Cut from caddy_2.11.8_checksums.txt (2026-11-04): sha512, two spaces.
    const CADDY: &str = "34d29142d6aeef494fdbf0c31805be896acfa223c6aaf7ed20b204b8ba836bedb73ff0362b12d1498c922512bc115d2608041754aba9c03e2319bda01505cf6b  caddy_2.11.8_mac_arm64.sbom\n\
        0c6992b1bb40629b6090d72cdc4a3f3722a7f6b667606db9fe67bfea0e42e9c40736d10925d406bf103628a5e906e59300d616a0709bc8855d9e85efbcfece81  caddy_2.11.8_mac_arm64.tar.gz\n\
        fff45b117b9d9ea5cc3f824f3c18de89630bcb43a0b759ca5e7163b7a3c1fb39ec7dae7f3147012d602213c950a46c8b0ef8095c77109e807ebf90132deeb759  caddy_2.11.8_windows_amd64.zip\n";
    // The whole of theseus-rs's postgresql-17.12.0-aarch64-apple-darwin.tar.gz.sha256
    // (2026-11-18): one line, two spaces, `\n`.
    const THESEUS: &str = "2f752033b208f9a6a12ef64a9ee23bf19149cff160841980db1e221bf7d24809  postgresql-17.12.0-aarch64-apple-darwin.tar.gz\n";
    const THESEUS_HASH: &str = "2f752033b208f9a6a12ef64a9ee23bf19149cff160841980db1e221bf7d24809";
    const THESEUS_NAME: &str = "postgresql-17.12.0-aarch64-apple-darwin.tar.gz";

    fn sha256_of(text: &str, name: &str) -> Result<Vec<u8>> {
        find_sum(text, name, Algorithm::Sha256)
    }

    #[test]
    fn reads_the_three_formats() {
        let node = sha256_of(NODE, "node-v22.23.4-darwin-arm64.tar.gz").expect("node");
        assert_eq!(
            hex(&node),
            "c9e1dbc748b33ed2d14f42445191ecaf14c56259c7a9a26d0042c49ec2b66428"
        );
        let php = sha256_of(PHP_WINDOWS, "php-8.4.27-1-nts-Win32-vs17-x64.zip").expect("php");
        assert_eq!(
            hex(&php),
            "dbae154a0a89791f36c3d48e68bae797878333e7d6cb5721e74617de59b63ab7"
        );
        let composer = sha256_of(COMPOSER, "composer.phar").expect("composer");
        assert_eq!(
            hex(&composer),
            "6f1b891e1d4927d6274027dc540fb490e47689e19e102c49e8da849794882f8c"
        );
    }

    #[test]
    fn reads_caddy_sha512() {
        let mac = find_sum(CADDY, "caddy_2.11.8_mac_arm64.tar.gz", Algorithm::Sha512)
            .expect("mac archive");
        assert_eq!(mac.len(), 64);
        assert_eq!(
            hex(&mac),
            "0c6992b1bb40629b6090d72cdc4a3f3722a7f6b667606db9fe67bfea0e42e9c4\
             0736d10925d406bf103628a5e906e59300d616a0709bc8855d9e85efbcfece81"
        );
        let windows = find_sum(CADDY, "caddy_2.11.8_windows_amd64.zip", Algorithm::Sha512)
            .expect("windows archive");
        assert!(hex(&windows).starts_with("fff45b117b9d"));
        // Only the exact name: the sbom of the same build is another line.
        assert_ne!(
            find_sum(CADDY, "caddy_2.11.8_mac_arm64.sbom", Algorithm::Sha512).expect("sbom"),
            mac
        );
    }

    #[test]
    fn a_hash_of_the_other_length_is_refused() {
        // A sha512 line read as sha256, and a sha256 line read as sha512.
        let err = sha256_of(CADDY, "caddy_2.11.8_mac_arm64.tar.gz").expect_err("too long");
        assert!(err.to_string().starts_with("bad sha256 for"), "{err}");
        let err = find_sum(COMPOSER, "composer.phar", Algorithm::Sha512).expect_err("too short");
        assert!(err.to_string().starts_with("bad sha512 for"), "{err}");
        // Not hex.
        let bad = format!("{}  caddy.tar.gz\n", "g".repeat(128));
        assert!(find_sum(&bad, "caddy.tar.gz", Algorithm::Sha512).is_err());
    }

    #[test]
    fn reads_a_theseus_sum() {
        assert_eq!(THESEUS.len(), 113, "the size GitHub lists for the asset");
        let sums = sum_file_for(&format!(
            "{THESEUS_RELEASES}17.12.0/postgresql-17.12.0-aarch64-apple-darwin.tar.gz"
        ))
        .expect("theseus publishes sums");
        assert_eq!(hex(&sums.find(THESEUS).expect("real file")), THESEUS_HASH);
        // Without the final newline, and with `\r\n`.
        for text in [THESEUS.trim_end().to_owned(), THESEUS.replace('\n', "\r\n")] {
            let sum = single_sum(&text, THESEUS_NAME, Algorithm::Sha256).expect("one line");
            assert_eq!(hex(&sum), THESEUS_HASH);
        }
    }

    #[test]
    fn bad_theseus_sums_are_refused() {
        let line = |hash: &str, name: &str| format!("{hash}  {name}\n");
        let cases = [
            (String::new(), "is empty"),
            ("\n".to_owned(), "is empty"),
            (line(&THESEUS_HASH[..63], THESEUS_NAME), "bad sha256"),
            (
                line(&format!("{THESEUS_HASH}0"), THESEUS_NAME),
                "bad sha256",
            ),
            (
                line(&THESEUS_HASH.to_uppercase(), THESEUS_NAME),
                "bad sha256",
            ),
            (line(&"g".repeat(64), THESEUS_NAME), "bad sha256"),
            (format!("{THESEUS}{THESEUS}"), "more than one line"),
            (format!("{THESEUS}\n"), "more than one line"),
            (
                format!("{THESEUS_HASH} {THESEUS_NAME}\n"),
                "is not `<hex>  <name>`",
            ),
            (THESEUS_HASH.to_owned(), "is not `<hex>  <name>`"),
            (line(THESEUS_HASH, &format!("*{THESEUS_NAME}")), "not for"),
            (line(THESEUS_HASH, &format!(" {THESEUS_NAME}")), "not for"),
            (
                line(
                    THESEUS_HASH,
                    "postgresql-17.12.0-x86_64-apple-darwin.tar.gz",
                ),
                "not for",
            ),
            (line(THESEUS_HASH, ""), "not for"),
        ];
        for (text, expected) in cases {
            let err = single_sum(&text, THESEUS_NAME, Algorithm::Sha256).expect_err(&text);
            assert!(err.to_string().contains(expected), "{text:?}: {err}");
        }
        // A list with our line among others is not what theseus publishes.
        let list = format!("{THESEUS}{}", NODE.replace("\r", ""));
        assert!(single_sum(&list, THESEUS_NAME, Algorithm::Sha256).is_err());
    }

    #[test]
    fn missing_name_is_an_error() {
        for (text, name) in [
            (NODE, "node-v22.23.4-linux-x64.tar.gz"),
            (PHP_WINDOWS, "php-8.4.27-nts-Win32-vs17-x64.zip"),
            (COMPOSER, "composer.phar.sig"),
            ("", "composer.phar"),
            (CADDY, "caddy_2.11.8_linux_amd64.tar.gz"),
        ] {
            let err = sha256_of(text, name).expect_err(name);
            assert!(err.to_string().contains("is not in"), "{err}");
        }
    }

    #[test]
    fn sum_file_urls() {
        use Algorithm::{Sha256, Sha512};
        let cases = [
            (
                "https://nodejs.org/dist/v22.23.4/node-v22.23.4-win-x64.zip",
                Some((
                    "https://nodejs.org/dist/v22.23.4/SHASUMS256.txt",
                    "node-v22.23.4-win-x64.zip",
                    Sha256,
                )),
            ),
            (
                "https://windows.php.net/downloads/releases/php-8.4.27-1-nts-Win32-vs17-x64.zip",
                Some((
                    "https://windows.php.net/downloads/releases/sha256sum.txt",
                    "php-8.4.27-1-nts-Win32-vs17-x64.zip",
                    Sha256,
                )),
            ),
            (
                "https://getcomposer.org/download/2.11.1/composer.phar",
                Some((
                    "https://getcomposer.org/download/2.11.1/composer.phar.sha256sum",
                    "composer.phar",
                    Sha256,
                )),
            ),
            (
                "https://windows.php.net/downloads/releases/archives/php-8.4.23-nts-Win32-vs17-x64.zip",
                None,
            ),
            (
                "https://github.com/XJOC/marsa/releases/download/php-8.4.27-r1/php-8.4.27-linux-x64.tar.gz",
                Some((
                    "https://github.com/XJOC/marsa/releases/download/php-8.4.27-r1/sha256sums.txt",
                    "php-8.4.27-linux-x64.tar.gz",
                    Sha256,
                )),
            ),
            (
                "https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_mac_arm64.tar.gz",
                Some((
                    "https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_checksums.txt",
                    "caddy_2.11.8_mac_arm64.tar.gz",
                    Sha512,
                )),
            ),
            (
                "https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_windows_amd64.zip",
                Some((
                    "https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_checksums.txt",
                    "caddy_2.11.8_windows_amd64.zip",
                    Sha512,
                )),
            ),
            // Not a release asset: no tag, or a folder below the tag.
            (
                "https://github.com/caddyserver/caddy/releases/download/v/caddy.tar.gz",
                None,
            ),
            (
                "https://github.com/caddyserver/caddy/releases/download/v2.11.8/x/caddy.tar.gz",
                None,
            ),
            (
                "https://dl.static-php.dev/static-php-cli/bulk/php-8.4.23-cli-macos-aarch64.tar.gz",
                None,
            ),
            (
                "https://github.com/XJOC/marsa/releases/download/runtimes-manifest/manifest.json",
                None,
            ),
        ];
        for (url, expected) in cases {
            let got = sum_file_for(url);
            let expected = expected.map(|(sums, name, algorithm)| SumFile {
                url: sums.to_owned(),
                name: name.to_owned(),
                algorithm,
                layout: Layout::List,
            });
            assert_eq!(got, expected, "{url}");
        }
    }

    #[test]
    fn theseus_sum_file_urls() {
        let url = format!("{THESEUS_RELEASES}18.7.0/postgresql-18.7.0-x86_64-pc-windows-msvc.zip");
        assert_eq!(
            sum_file_for(&url),
            Some(SumFile {
                url: format!("{url}.sha256"),
                name: "postgresql-18.7.0-x86_64-pc-windows-msvc.zip".to_owned(),
                algorithm: Algorithm::Sha256,
                layout: Layout::Single,
            })
        );
        // Not a release asset: no tag, a folder below the tag, no file name,
        // or another repository.
        for url in [
            format!("{THESEUS_RELEASES}postgresql-18.7.0-x86_64-pc-windows-msvc.zip"),
            format!("{THESEUS_RELEASES}/postgresql-18.7.0-x86_64-pc-windows-msvc.zip"),
            format!("{THESEUS_RELEASES}18.7.0/x/postgresql-18.7.0-x86_64-pc-windows-msvc.zip"),
            format!("{THESEUS_RELEASES}18.7.0/"),
            "https://github.com/theseus-rs/postgresql-binaries-fork/releases/download/18.7.0/postgresql-18.7.0-x86_64-pc-windows-msvc.zip".to_owned(),
        ] {
            assert_eq!(sum_file_for(&url), None, "{url}");
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
