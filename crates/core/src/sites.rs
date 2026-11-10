//! Parked and linked folders, and project type detection.
//!
//! - A **parked** folder: every sub-folder in it becomes a site, named after
//!   the sub-folder (like `valet park`).
//! - A **linked** folder: one folder under a name the user picks (like
//!   `valet link`).
//!
//! The functions here read folder lists and check that a few files exist.
//! They never write and never start anything. The daemon stores the folder
//! lists, watches them, and picks the PHP version of each site.
//!
//! Not behind the `full` feature: no tokio, no network.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::resolver::{Resolution, Source};

/// The top-level domain of every site: `shop` is served at `https://shop.test`.
pub const TLD: &str = "test";

/// The longest site name: one DNS label.
pub const MAX_NAME_LEN: usize = 63;

/// One site, ready for the Caddy config and the site list in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    /// The name in the URL: `shop` → `https://shop.test`. Always valid
    /// (see [`validate_name`]).
    pub name: String,
    /// The project folder.
    pub root: PathBuf,
    /// The folder that the web server serves: `root/public` for Laravel.
    /// Found at detection time, so the Caddy config never reads the disk.
    pub docroot: PathBuf,
    pub kind: SiteKind,
    /// The PHP version picked for this site. `None`: no PHP is installed, or
    /// none matches what the site asks for.
    #[serde(with = "optional_version")]
    pub php: Option<Version>,
    /// Where `php` came from (task M5-02), so the UI can say why it was
    /// picked: the site's own setting, a version file, `composer.json`, or
    /// the global default. `None` exactly when `php` is `None`. Missing in
    /// JSON from an older daemon reads as `None`.
    #[serde(default)]
    pub php_source: Option<Source>,
    /// Served over HTTPS. The default is `true`.
    pub secure: bool,
}

impl Site {
    /// The host name: `shop.test`.
    pub fn host(&self) -> String {
        format!("{}.{TLD}", self.name)
    }
}

/// What kind of project a site is. It decides how the web server serves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SiteKind {
    /// `artisan` and `public/index.php`.
    Laravel,
    /// Any other PHP project: `public/index.php`, `index.php`, or WordPress.
    Php,
    /// Plain files, served as they are.
    Static,
    /// A program the user runs, for example `npm run dev`. The web server
    /// forwards requests to `127.0.0.1:<port>`. Never detected: the user sets
    /// the port.
    NodeProxy { port: u16 },
}

/// The result of [`detect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub kind: SiteKind,
    pub docroot: PathBuf,
}

/// A folder that will become a site. The daemon adds the PHP version and the
/// HTTPS setting with [`Found::into_site`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub root: PathBuf,
    pub detected: Detected,
}

impl Found {
    /// Builds the [`Site`] with the settings that come from the daemon: the
    /// PHP version the resolver picked, if any, and the HTTPS setting.
    pub fn into_site(self, php: Option<Resolution>, secure: bool) -> Site {
        let (php, php_source) = match php {
            Some(resolution) => (Some(resolution.version), Some(resolution.source)),
            None => (None, None),
        };
        Site {
            name: self.name,
            root: self.root,
            docroot: self.detected.docroot,
            kind: self.detected.kind,
            php,
            php_source,
            secure,
        }
    }
}

/// A folder that did not become a site, and why. The UI shows these, so the
/// user knows why a folder is missing from the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The folder name gives no valid site name (for example, it has no
    /// ASCII letters or digits).
    NameInvalid,
    /// The path is not valid UTF-8, so it cannot be written in the Caddy
    /// config.
    PathNotUtf8,
    /// Another folder already has this site name.
    NameTaken { name: String, by: PathBuf },
}

impl SkipReason {
    /// Stable name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            SkipReason::NameInvalid => "site_name_invalid",
            SkipReason::PathNotUtf8 => "site_path_not_utf8",
            SkipReason::NameTaken { .. } => "site_name_taken",
        }
    }
}

/// The sites found in parked and linked folders, sorted by name, and the
/// folders that were skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub found: Vec<Found>,
    pub skipped: Vec<Skipped>,
}

/// Why a folder could not be read or linked. [`SiteError::code`] gives a
/// stable name that the UI maps to a message.
#[derive(Debug, thiserror::Error)]
pub enum SiteError {
    #[error(
        "{name:?} is not a valid site name: use 1 to 63 of a-z, 0-9, and -, not at the start or end"
    )]
    NameInvalid { name: String },

    #[error("cannot read {}", .path.display())]
    ReadDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{} is not a folder", .path.display())]
    NotAFolder { path: PathBuf },

    #[error("{} is not valid UTF-8", .path.display())]
    PathNotUtf8 { path: PathBuf },
}

impl SiteError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            SiteError::NameInvalid { .. } => "site_name_invalid",
            SiteError::ReadDir { .. } => "site_read_dir",
            SiteError::NotAFolder { .. } => "site_not_a_folder",
            SiteError::PathNotUtf8 { .. } => "site_path_not_utf8",
        }
    }
}

/// Checks a site name: 1 to 63 characters of `a-z`, `0-9`, and `-`, with no
/// `-` at the start or the end. These are the rules of one DNS label, and the
/// helper accepts the same names on Windows.
///
/// # Errors
///
/// `site_name_invalid`.
pub fn validate_name(name: &str) -> Result<(), SiteError> {
    let allowed = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let valid = allowed
        && (1..=MAX_NAME_LEN).contains(&name.len())
        && !name.starts_with('-')
        && !name.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(SiteError::NameInvalid {
            name: name.to_string(),
        })
    }
}

/// Turns a folder name into a site name, or `None` if nothing valid is left.
///
/// ASCII letters become lowercase. A space, `_`, `.`, or `-` becomes `-`.
/// Every other character is dropped. Repeated `-` become one, and `-` is cut
/// from both ends. `My_App.v2` → `my-app-v2`.
pub fn name_from_folder(folder: &OsStr) -> Option<String> {
    let text = folder.to_str()?;
    let mut name = String::with_capacity(text.len());
    for c in text.chars() {
        let next = match c {
            'a'..='z' | '0'..='9' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            ' ' | '_' | '.' | '-' => '-',
            _ => continue,
        };
        if next == '-' && (name.is_empty() || name.ends_with('-')) {
            continue;
        }
        name.push(next);
    }
    while name.ends_with('-') {
        name.pop();
    }
    validate_name(&name).ok().map(|()| name)
}

/// Finds the project type and the folder to serve. The first rule that
/// matches wins:
///
/// | Files | Kind | Docroot |
/// | --- | --- | --- |
/// | `artisan` and `public/index.php` | Laravel | `public` |
/// | `public/index.php` | Php | `public` |
/// | `index.php`, `wp-config.php`, or `wp-load.php` | Php | the root |
/// | `public/index.html` | Static | `public` |
/// | anything else | Static | the root |
pub fn detect(root: &Path) -> Detected {
    let public = root.join("public");
    let has = |path: &Path| path.is_file();
    let (kind, docroot) = if has(&root.join("artisan")) && has(&public.join("index.php")) {
        (SiteKind::Laravel, public)
    } else if has(&public.join("index.php")) {
        (SiteKind::Php, public)
    } else if ["index.php", "wp-config.php", "wp-load.php"]
        .iter()
        .any(|file| has(&root.join(file)))
    {
        (SiteKind::Php, root.to_path_buf())
    } else if has(&public.join("index.html")) {
        (SiteKind::Static, public)
    } else {
        (SiteKind::Static, root.to_path_buf())
    };
    Detected { kind, docroot }
}

/// Lists the sites in one parked folder: one per direct sub-folder.
///
/// - Files and hidden entries (a name that starts with `.`) are ignored and
///   not reported: they are never meant to be sites.
/// - A symlink to a folder counts as a folder. Nothing is canonicalized.
/// - Sub-folders are visited in byte order of their names. If two give the
///   same site name (`Shop` and `shop`), the first wins and the second is
///   skipped with `site_name_taken`.
///
/// # Errors
///
/// `site_read_dir` if the folder or one of its entries cannot be read.
pub fn scan_parked(dir: &Path) -> Result<Scan, SiteError> {
    let read_error = |source| SiteError::ReadDir {
        path: dir.to_path_buf(),
        source,
    };
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).map_err(read_error)? {
        let entry = entry.map_err(read_error)?;
        let hidden = entry.file_name().as_encoded_bytes().starts_with(b".");
        let path = entry.path();
        if !hidden && path.is_dir() {
            paths.push(path);
        }
    }
    paths.sort();

    let mut scan = Scan::default();
    let mut taken: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in paths {
        if path.to_str().is_none() {
            scan.skipped.push(Skipped {
                path,
                reason: SkipReason::PathNotUtf8,
            });
            continue;
        }
        let Some(name) = path.file_name().and_then(name_from_folder) else {
            scan.skipped.push(Skipped {
                path,
                reason: SkipReason::NameInvalid,
            });
            continue;
        };
        if let Some(by) = taken.get(&name) {
            scan.skipped.push(Skipped {
                path,
                reason: SkipReason::NameTaken {
                    name,
                    by: by.clone(),
                },
            });
            continue;
        }
        taken.insert(name.clone(), path.clone());
        let detected = detect(&path);
        scan.found.push(Found {
            name,
            root: path,
            detected,
        });
    }
    scan.found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(scan)
}

/// Checks one linked folder and detects its type.
///
/// # Errors
///
/// `site_name_invalid`, `site_not_a_folder`, or `site_path_not_utf8`.
pub fn link(name: &str, root: &Path) -> Result<Found, SiteError> {
    validate_name(name)?;
    if root.to_str().is_none() {
        return Err(SiteError::PathNotUtf8 {
            path: root.to_path_buf(),
        });
    }
    if !root.is_dir() {
        return Err(SiteError::NotAFolder {
            path: root.to_path_buf(),
        });
    }
    Ok(Found {
        name: name.to_string(),
        root: root.to_path_buf(),
        detected: detect(root),
    })
}

/// Joins linked folders and parked scans into one list.
///
/// Linked folders come first, so a linked site wins over a parked folder with
/// the same name. After that, parked scans are taken in the order given. The
/// result is sorted by name, and every skipped folder from the inputs is kept.
pub fn merge(linked: Vec<Found>, parked: Vec<Scan>) -> Scan {
    let mut merged = Scan::default();
    let mut taken: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut candidates = linked;
    for scan in parked {
        candidates.extend(scan.found);
        merged.skipped.extend(scan.skipped);
    }
    for found in candidates {
        if let Some(by) = taken.get(&found.name) {
            merged.skipped.push(Skipped {
                path: found.root,
                reason: SkipReason::NameTaken {
                    name: found.name,
                    by: by.clone(),
                },
            });
            continue;
        }
        taken.insert(found.name.clone(), found.root.clone());
        merged.found.push(found);
    }
    merged.found.sort_by(|a, b| a.name.cmp(&b.name));
    merged
}

/// `Option<Version>` as `"8.4.1"` or `null` in JSON. `semver` is built
/// without its `serde` feature, and the shim must stay small.
mod optional_version {
    use semver::Version;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Version>, s: S) -> Result<S::Ok, S::Error> {
        value.as_ref().map(Version::to_string).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Version>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|text| Version::parse(&text).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use tempfile::TempDir;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join(name)
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, "").expect("write file");
    }

    fn names(scan: &Scan) -> Vec<&str> {
        scan.found.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn valid_names() {
        let long = "a".repeat(63);
        for name in ["a", "shop", "my-app-2", "0", long.as_str()] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn invalid_names() {
        let long = "a".repeat(64);
        for name in [
            "",
            "-shop",
            "shop-",
            "Shop",
            "my_app",
            "a.b",
            "a b",
            "متجر",
            long.as_str(),
        ] {
            let err = validate_name(name).expect_err(name);
            assert_eq!(err.code(), "site_name_invalid");
        }
    }

    #[test]
    fn names_from_folders() {
        let cases = [
            ("Shop", Some("shop")),
            ("my_app.v2", Some("my-app-v2")),
            ("--x--", Some("x")),
            ("a  b", Some("a-b")),
            ("my@app", Some("myapp")),
            ("متجر", None),
            ("---", None),
            ("", None),
        ];
        for (folder, expected) in cases {
            assert_eq!(
                name_from_folder(OsStr::new(folder)).as_deref(),
                expected,
                "{folder:?}"
            );
        }
    }

    #[test]
    fn name_length_limit() {
        let ok = "A".repeat(63);
        assert_eq!(name_from_folder(OsStr::new(&ok)), Some("a".repeat(63)));
        assert_eq!(name_from_folder(OsStr::new(&"a".repeat(64))), None);
    }

    #[test]
    fn detects_fixtures() {
        let laravel = fixture("laravel");
        assert_eq!(
            detect(&laravel),
            Detected {
                kind: SiteKind::Laravel,
                docroot: laravel.join("public"),
            }
        );
        for name in ["wordpress", "no-version"] {
            let root = fixture(name);
            assert_eq!(
                detect(&root),
                Detected {
                    kind: SiteKind::Php,
                    docroot: root.clone(),
                },
                "{name}"
            );
        }
        let node = fixture("node");
        assert_eq!(
            detect(&node),
            Detected {
                kind: SiteKind::Static,
                docroot: node.clone(),
            }
        );
    }

    #[test]
    fn detects_public_php_and_static() {
        let dir = TempDir::new().expect("temp dir");
        let php = dir.path().join("php");
        touch(&php.join("public").join("index.php"));
        assert_eq!(detect(&php).kind, SiteKind::Php);
        assert_eq!(detect(&php).docroot, php.join("public"));

        // `artisan` alone is not enough: Laravel needs public/index.php.
        let half = dir.path().join("half");
        touch(&half.join("artisan"));
        assert_eq!(detect(&half).kind, SiteKind::Static);

        let site = dir.path().join("site");
        touch(&site.join("public").join("index.html"));
        assert_eq!(
            detect(&site),
            Detected {
                kind: SiteKind::Static,
                docroot: site.join("public"),
            }
        );
    }

    #[test]
    fn scan_parked_folder() {
        let dir = TempDir::new().expect("temp dir");
        let p = dir.path();
        touch(&p.join("Shop").join("artisan"));
        touch(&p.join("Shop").join("public").join("index.php"));
        fs::create_dir(p.join("blog")).expect("mkdir");
        fs::create_dir(p.join(".git")).expect("mkdir");
        fs::create_dir(p.join("متجر")).expect("mkdir");
        touch(&p.join("notes.txt"));

        let scan = scan_parked(p).expect("scan");
        assert_eq!(names(&scan), ["blog", "shop"]);
        let shop = &scan.found[1];
        assert_eq!(shop.root, p.join("Shop"));
        assert_eq!(shop.detected.kind, SiteKind::Laravel);
        assert_eq!(
            scan.skipped,
            [Skipped {
                path: p.join("متجر"),
                reason: SkipReason::NameInvalid,
            }]
        );
    }

    #[test]
    fn same_name_first_wins() {
        let dir = TempDir::new().expect("temp dir");
        let p = dir.path();
        fs::create_dir(p.join("My App")).expect("mkdir");
        fs::create_dir(p.join("my_app")).expect("mkdir");

        let scan = scan_parked(p).expect("scan");
        assert_eq!(names(&scan), ["my-app"]);
        assert_eq!(scan.found[0].root, p.join("My App"));
        assert_eq!(scan.skipped.len(), 1);
        assert_eq!(scan.skipped[0].path, p.join("my_app"));
        assert_eq!(scan.skipped[0].reason.code(), "site_name_taken");
    }

    #[test]
    fn missing_parked_folder() {
        let dir = TempDir::new().expect("temp dir");
        let err = scan_parked(&dir.path().join("nope")).expect_err("missing");
        assert_eq!(err.code(), "site_read_dir");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_folder_is_a_site() {
        let dir = TempDir::new().expect("temp dir");
        let real = dir.path().join("real");
        touch(&real.join("index.php"));
        let parked = dir.path().join("parked");
        fs::create_dir(&parked).expect("mkdir");
        std::os::unix::fs::symlink(&real, parked.join("linked")).expect("symlink");

        let scan = scan_parked(&parked).expect("scan");
        assert_eq!(names(&scan), ["linked"]);
        assert_eq!(scan.found[0].root, parked.join("linked"));
        assert_eq!(scan.found[0].detected.kind, SiteKind::Php);
    }

    // macOS (APFS) refuses file names that are not UTF-8, and Windows names
    // are UTF-16, so this runs on Linux only.
    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_folder_is_skipped() {
        use std::os::unix::ffi::OsStringExt;
        let dir = TempDir::new().expect("temp dir");
        let bad = dir.path().join(OsString::from_vec(b"bad\xff".to_vec()));
        fs::create_dir(&bad).expect("mkdir");

        let scan = scan_parked(dir.path()).expect("scan");
        assert!(scan.found.is_empty());
        assert_eq!(scan.skipped[0].reason, SkipReason::PathNotUtf8);
    }

    #[test]
    fn non_utf8_name_gives_none() {
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(b"shop\xff".to_vec())
        };
        #[cfg(windows)]
        let name = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0x73, 0xD800])
        };
        assert_eq!(name_from_folder(&name), None);
    }

    #[test]
    fn link_checks_name_and_folder() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path().join("app");
        touch(&root.join("index.php"));

        let found = link("app", &root).expect("link");
        assert_eq!(found.detected.kind, SiteKind::Php);

        assert_eq!(
            link("Bad Name", &root).expect_err("name").code(),
            "site_name_invalid"
        );
        assert_eq!(
            link("app", &root.join("index.php"))
                .expect_err("file")
                .code(),
            "site_not_a_folder"
        );
    }

    #[test]
    fn linked_wins_over_parked() {
        let dir = TempDir::new().expect("temp dir");
        let parked = dir.path().join("parked");
        fs::create_dir_all(parked.join("shop")).expect("mkdir");
        fs::create_dir_all(parked.join("blog")).expect("mkdir");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir(&elsewhere).expect("mkdir");

        let linked = vec![link("shop", &elsewhere).expect("link")];
        let merged = merge(linked, vec![scan_parked(&parked).expect("scan")]);

        assert_eq!(names(&merged), ["blog", "shop"]);
        assert_eq!(merged.found[1].root, elsewhere);
        assert_eq!(
            merged.skipped,
            [Skipped {
                path: parked.join("shop"),
                reason: SkipReason::NameTaken {
                    name: "shop".to_string(),
                    by: elsewhere.clone(),
                },
            }]
        );
    }

    #[test]
    fn site_json_shape() {
        let found = Found {
            name: "shop".to_string(),
            root: PathBuf::from("/code/shop"),
            detected: Detected {
                kind: SiteKind::NodeProxy { port: 3000 },
                docroot: PathBuf::from("/code/shop"),
            },
        };
        let resolution = Resolution {
            tool: crate::resolver::Tool::Php,
            version: Version::new(8, 4, 1),
            source: Source::VersionFile(PathBuf::from("/code/shop/.php-version")),
        };
        let site = found.into_site(Some(resolution), true);
        assert_eq!(site.host(), "shop.test");

        let json = serde_json::to_value(&site).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "name": "shop",
                "root": "/code/shop",
                "docroot": "/code/shop",
                "kind": { "type": "node_proxy", "port": 3000 },
                "php": "8.4.1",
                "php_source": { "type": "version_file", "path": "/code/shop/.php-version" },
                "secure": true,
            })
        );
        let back: Site = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, site);

        // The sources with no path, and with the manifest's path.
        for (source, want) in [
            (
                Source::ProjectSetting,
                serde_json::json!({ "type": "project_setting" }),
            ),
            (
                Source::GlobalDefault,
                serde_json::json!({ "type": "global_default" }),
            ),
            (
                Source::Constraint(PathBuf::from("/code/shop/composer.json")),
                serde_json::json!({ "type": "constraint", "path": "/code/shop/composer.json" }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&source).expect("serialize"), want);
            assert_eq!(
                serde_json::from_value::<Source>(want).expect("back"),
                source
            );
        }

        let none = Site {
            php: None,
            php_source: None,
            ..site
        };
        let json = serde_json::to_value(&none).expect("serialize");
        assert_eq!(json["php"], serde_json::Value::Null);
        assert_eq!(json["php_source"], serde_json::Value::Null);
        assert_eq!(serde_json::from_value::<Site>(json).expect("back"), none);

        // An older daemon sends no `php_source`.
        let mut old = serde_json::to_value(&none).expect("serialize");
        old.as_object_mut().expect("an object").remove("php_source");
        assert_eq!(serde_json::from_value::<Site>(old).expect("back"), none);
    }
}
