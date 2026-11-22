//! Oracle's proof for a MySQL archive (ADR 0023, section 3; task M6-02):
//! its GPG signature, by the key whose primary fingerprint is pinned here,
//! and its MD5. Both, never one.
//!
//! The signature is checked with the `gpg` program, found by absolute path,
//! in a new home folder that is deleted after: the owner's own keyring is
//! never read or changed. Oracle's key file in the repository becomes a
//! keyring there (`gpg --dearmor`), and only gpg's machine-readable status
//! lines (`--status-fd 1`) are read, by [`check_status`]. The MD5 comes from
//! the system's `md5` program (decided 2026-11-18: `xtask manifest build`
//! runs only on the owner's Mac, and no MD5 crate is approved).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail, ensure};

use super::build::{ArchiveKind, OracleCheck};
use super::upstream::parse_hex_bytes;

/// The primary fingerprint of Oracle's MySQL release key, "MySQL Release
/// Engineering <mysql-build@oss.oracle.com>". A signature by any other key
/// is refused. A key file with a new fingerprint needs the owner.
pub(super) const ORACLE_FINGERPRINT: &str = "BCA43417C3B485DD128EC6D4B7B3B788A8D3785C";

/// Oracle's key file, from the workspace root: a copy of
/// `https://repo.mysql.com/RPM-GPG-KEY-mysql-2025`. It expires on
/// 2027-10-23; Oracle's renewed file with the same fingerprint replaces it
/// in a reviewed change.
pub(super) const ORACLE_KEY_FILE: &str = "recipes/keys/mysql-2025.asc";

/// Where `gpg` may be. Never searched in `PATH`.
const GPG_PATHS: [&str; 3] = [
    "/opt/homebrew/bin/gpg",
    "/usr/local/bin/gpg",
    "/usr/bin/gpg",
];

/// macOS's own `md5`. `-q` prints the hex only.
const MD5_PATH: &str = "/sbin/md5";

/// The `gpg` program, from [`GPG_PATHS`].
///
/// # Errors
///
/// When none is there: the check is never skipped.
pub(super) fn find_gpg() -> Result<PathBuf> {
    find_gpg_in(&GPG_PATHS)
}

/// The first of `paths` that is a file.
fn find_gpg_in(paths: &[&str]) -> Result<PathBuf> {
    paths
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "no gpg program at {}: install GnuPG (`brew install gnupg`). Oracle's signature \
                 on MySQL is never skipped",
                paths.join(", ")
            )
        })
}

/// Checks that `signature`, a detached `.asc`, is a good signature on
/// `file` by the key with the primary fingerprint `fingerprint`, from the
/// keys in `key_file`. `home` is a folder for gpg that is made empty here
/// and deleted after.
///
/// # Errors
///
/// gpg cannot run or read the key file, or [`check_status`] refuses its
/// answer, or gpg failed after all.
pub(super) fn verify(
    gpg: &Path,
    key_file: &Path,
    signature: &Path,
    file: &Path,
    home: &Path,
    fingerprint: &str,
) -> Result<()> {
    remove_dir(home)?;
    make_private_dir(home)?;
    let result = verify_in(gpg, key_file, signature, file, home, fingerprint);
    if let Err(err) = remove_dir(home) {
        println!("    warning: {err:#}");
    }
    result
}

fn verify_in(
    gpg: &Path,
    key_file: &Path,
    signature: &Path,
    file: &Path,
    home: &Path,
    fingerprint: &str,
) -> Result<()> {
    let keyring = home.join("oracle.gpg");
    let mut dearmor = gpg_command(gpg, home);
    dearmor
        .arg("--dearmor")
        .arg("--output")
        .arg(&keyring)
        .arg(key_file);
    let out = run(dearmor, "gpg --dearmor")?;
    ensure!(
        out.status.success() && keyring.is_file(),
        "gpg --dearmor {} failed ({}): {}",
        key_file.display(),
        out.status,
        stderr_of(&out)
    );

    let mut check = gpg_command(gpg, home);
    check
        .args(["--no-default-keyring", "--keyring"])
        .arg(&keyring)
        .args(["--status-fd", "1", "--verify"])
        .arg(signature)
        .arg(file);
    let out = run(check, "gpg --verify")?;
    let status = String::from_utf8_lossy(&out.stdout);
    check_status(&status, fingerprint).with_context(|| {
        format!(
            "{} is not a good signature on {} by {fingerprint}",
            signature.display(),
            file.display()
        )
    })?;
    // gpg exits 0 for a signature by an expired key too, so the status lines
    // decide; this only catches a failure they did not show.
    ensure!(
        out.status.success(),
        "gpg --verify exited with {} after a good signature: {}",
        out.status,
        stderr_of(&out)
    );
    Ok(())
}

/// gpg in its own `home`, with no prompt, no terminal, no agent, no
/// options file (not even Homebrew's global one), and no key lookups on
/// the network.
fn gpg_command(gpg: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(gpg);
    cmd.arg("--homedir").arg(home).args([
        "--no-options",
        "--batch",
        "--no-tty",
        "--no-autostart",
        "--no-auto-key-retrieve",
    ]);
    cmd
}

fn run(mut cmd: Command, what: &str) -> Result<Output> {
    cmd.output().with_context(|| format!("cannot run {what}"))
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_owned()
}

/// Accepts gpg's `--status-fd` lines for one detached signature only when
/// they say: exactly one signature, `GOODSIG`, and one `VALIDSIG` of class
/// `00` (binary: the exact bytes, not text with its line ends normalized)
/// whose last field, the **primary** key's fingerprint, is `fingerprint`. The first
/// field is the key that signed, which may be a subkey, so it is not the one
/// compared. Any sign of a bad, expired, revoked, or unknown key refuses.
///
/// # Errors
///
/// Why the lines do not prove a good signature by that key.
pub(super) fn check_status(text: &str, fingerprint: &str) -> Result<()> {
    let lines: Vec<(&str, Vec<&str>)> = text
        .lines()
        .filter_map(|line| line.trim_end_matches('\r').strip_prefix("[GNUPG:] "))
        .map(|rest| {
            let mut fields = rest.split(' ');
            let keyword = fields.next().unwrap_or_default();
            (keyword, fields.collect())
        })
        .collect();
    let count = |keyword: &str| lines.iter().filter(|(k, _)| *k == keyword).count();
    let has = |keywords: &[&str]| keywords.iter().any(|keyword| count(keyword) > 0);

    if has(&["NODATA"]) || count("NEWSIG") == 0 {
        bail!("gpg found no signature in it");
    }
    let signatures = count("NEWSIG");
    ensure!(
        signatures == 1,
        "it holds {signatures} signatures, and only one is accepted"
    );
    if has(&["BADSIG"]) {
        bail!("the file does not match the signature (BADSIG): it changed after it was signed");
    }
    // Not `KEYEXPIRED`: gpg prints it for an expired subkey that did not
    // sign too.
    if has(&["EXPKEYSIG"]) {
        bail!(
            "the key in the key file has expired (EXPKEYSIG): replace the key file with the \
             signer's renewed one, of the same fingerprint {fingerprint}"
        );
    }
    if has(&["REVKEYSIG", "KEYREVOKED"]) {
        bail!("the signing key is revoked (REVKEYSIG)");
    }
    if has(&["EXPSIG"]) {
        bail!("the signature has expired (EXPSIG)");
    }
    if has(&["ERRSIG", "NO_PUBKEY"]) {
        bail!("it was made by a key that is not in the key file (ERRSIG)");
    }
    if has(&["FAILURE", "ERROR"]) {
        bail!("gpg reported a failure");
    }
    ensure!(
        count("GOODSIG") == 1,
        "gpg did not call it good (no GOODSIG)"
    );
    let valid: Vec<&Vec<&str>> = lines
        .iter()
        .filter(|(k, _)| *k == "VALIDSIG")
        .map(|(_, fields)| fields)
        .collect();
    let [fields] = valid.as_slice() else {
        bail!("gpg gave {} VALIDSIG lines, not one", valid.len());
    };
    // `<signing key> <date> <time> <expires> <version> <reserved> <key
    // algorithm> <hash algorithm> <class> <primary key>`.
    let [signer, _, _, _, _, _, _, _, class, primary] = fields.as_slice() else {
        bail!(
            "the VALIDSIG line has {} fields, not 10, so no primary key fingerprint",
            fields.len()
        );
    };
    ensure!(
        *primary == fingerprint,
        "it was made by the key {signer} of the primary key {primary}, not by {fingerprint}"
    );
    ensure!(
        *class == "00",
        "its class is {class}, not 00: it does not sign the exact bytes"
    );
    Ok(())
}

/// The MD5 of `file`, from [`MD5_PATH`].
///
/// # Errors
///
/// No such program (not a Mac), or it failed, or its answer is not an MD5.
pub(super) fn md5_of(file: &Path) -> Result<[u8; 16]> {
    ensure!(
        Path::new(MD5_PATH).is_file(),
        "no {MD5_PATH}: the MySQL check runs on macOS only"
    );
    let mut cmd = Command::new(MD5_PATH);
    cmd.arg("-q").arg(file);
    let out = run(cmd, MD5_PATH)?;
    ensure!(
        out.status.success(),
        "{MD5_PATH} -q {} failed ({}): {}",
        file.display(),
        out.status,
        stderr_of(&out)
    );
    parse_md5_output(&String::from_utf8_lossy(&out.stdout))
        .with_context(|| format!("{MD5_PATH} gave no MD5 for {}", file.display()))
}

/// `md5 -q`'s answer: 32 lowercase hex characters and a line end.
pub(super) fn parse_md5_output(text: &str) -> Option<[u8; 16]> {
    let line = text.strip_suffix('\n').unwrap_or(text);
    parse_hex_bytes(line, 16)?.try_into().ok()
}

/// Deletes `dir` and what is in it. Nothing there is not an error.
fn remove_dir(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(err).with_context(|| format!("cannot delete {}", dir.display()))
        }
        _ => Ok(()),
    }
}

/// A new folder that only the owner can open, as gpg wants its home.
fn make_private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    // `#[cfg(unix)]` outside `platform`: xtask has no platform module, and
    // only Unix has folder modes. gpg runs on macOS and Linux only.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))
}

/// The real programs: `gpg` with Oracle's key file, and `/sbin/md5`.
pub(super) struct System {
    /// The workspace root, for [`ORACLE_KEY_FILE`].
    pub root: PathBuf,
    /// gpg's home for each check.
    pub home: PathBuf,
}

impl OracleCheck for System {
    fn signature(&self, signature: &Path, file: &Path) -> Result<()> {
        let gpg = find_gpg()?;
        let key_file = self.root.join(ORACLE_KEY_FILE);
        verify(
            &gpg,
            &key_file,
            signature,
            file,
            &self.home,
            ORACLE_FINGERPRINT,
        )
    }

    fn md5(&self, file: &Path) -> Result<[u8; 16]> {
        md5_of(file)
    }

    fn first_entry(&self, file: &Path, kind: ArchiveKind) -> Result<String> {
        match kind {
            ArchiveKind::Tar => first_tar_entry(Path::new(TAR_PATH), file),
            ArchiveKind::Zip => first_zip_entry(Path::new(UNZIP_PATH), file),
        }
    }
}

/// macOS's own `tar` (bsdtar), which reads `.tar.gz` and `.tar.xz`. Never
/// searched in `PATH`.
const TAR_PATH: &str = "/usr/bin/tar";

/// macOS's own `unzip`, for Windows' `.zip`. Never searched in `PATH`.
const UNZIP_PATH: &str = "/usr/bin/unzip";

/// The first line of `unzip -Z1 <file>`, which lists one entry name per
/// line. Unlike a tar listing, it reads only the zip's central directory, so
/// the rest of the listing (a few hundred names) is read too, and the exit
/// code must be 0: `unzip` prints `Empty zipfile.` there for a zip with no
/// entry, and exits with 1. No environment, so `UNZIP` and `ZIPINFO` options
/// of the caller do not apply.
fn first_zip_entry(unzip: &Path, file: &Path) -> Result<String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let mut child = Command::new(unzip)
        .arg("-Z1")
        .arg(file)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot run {}", unzip.display()))?;
    let stdout = child.stdout.take().context("unzip has no output")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = reader.read_line(&mut line);
    let rest = std::io::copy(&mut reader, &mut std::io::sink());
    let status = child
        .wait()
        .with_context(|| format!("cannot wait for {}", unzip.display()))?;
    let read = read.with_context(|| format!("cannot read the listing of {}", file.display()))?;
    rest.with_context(|| format!("cannot read the listing of {}", file.display()))?;
    ensure!(
        status.success(),
        "{} -Z1 {} failed ({status})",
        unzip.display(),
        file.display()
    );
    ensure!(read > 0, "{} lists no entry", file.display());
    Ok(line.trim_end_matches(['\n', '\r']).to_owned())
}

/// The first line of `tar -tf <file>`, read without listing the rest: the
/// full archives have tens of thousands of entries.
fn first_tar_entry(tar: &Path, file: &Path) -> Result<String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let mut child = Command::new(tar)
        .arg("-tf")
        .arg(file)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot run {}", tar.display()))?;
    let stdout = child.stdout.take().context("tar has no output")?;
    let mut line = String::new();
    let read = BufReader::new(stdout).read_line(&mut line);
    // Not waiting for the end of the listing.
    let _ = child.kill();
    let _ = child.wait();
    let read = read.with_context(|| format!("cannot read the listing of {}", file.display()))?;
    ensure!(read > 0, "{} lists no entry", file.display());
    Ok(line.trim_end_matches(['\n', '\r']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first entry that the system's `tar` lists, from a real archive.
    #[cfg(unix)]
    #[test]
    fn the_first_tar_entry_is_read_from_a_real_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let top = dir.path().join("mysql-8.4.13-x");
        fs::create_dir_all(top.join("bin")).expect("dirs");
        fs::write(top.join("bin/mysqld"), b"x").expect("file");
        let archive = dir.path().join("a.tar.gz");
        let made = Command::new(TAR_PATH)
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.path())
            .arg("mysql-8.4.13-x")
            .status()
            .expect("tar runs");
        assert!(made.success());
        let first = first_tar_entry(Path::new(TAR_PATH), &archive).expect("listing");
        assert_eq!(first.split('/').next(), Some("mysql-8.4.13-x"), "{first}");

        let empty = dir.path().join("empty.tar");
        fs::write(&empty, vec![0_u8; 1024]).expect("empty tar");
        assert!(first_tar_entry(Path::new(TAR_PATH), &empty).is_err());
        assert!(first_tar_entry(Path::new(TAR_PATH), &dir.path().join("none")).is_err());
        assert!(first_tar_entry(&dir.path().join("no-tar"), &archive).is_err());
    }

    /// The first entry that the system's `unzip` lists, from a real zip as
    /// Oracle makes it for Windows: one top folder.
    #[cfg(unix)]
    #[test]
    fn the_first_zip_entry_is_read_from_a_real_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let top = dir.path().join("mysql-8.4.13-winx64");
        fs::create_dir_all(top.join("bin")).expect("dirs");
        fs::write(top.join("bin/mysqld.exe"), b"x").expect("file");
        let archive = dir.path().join("a.zip");
        let made = Command::new("/usr/bin/zip")
            .arg("-q")
            .arg("-r")
            .arg(&archive)
            .arg("mysql-8.4.13-winx64")
            .current_dir(dir.path())
            .status()
            .expect("zip runs");
        assert!(made.success());
        let unzip = Path::new(UNZIP_PATH);
        let first = first_zip_entry(unzip, &archive).expect("listing");
        assert_eq!(
            first.split('/').next(),
            Some("mysql-8.4.13-winx64"),
            "{first}"
        );

        // A zip with no entry: `unzip` says so on stdout, and exits with 1.
        let empty = dir.path().join("empty.zip");
        let end_of_directory = [b"PK\x05\x06".as_slice(), &[0_u8; 18]].concat();
        fs::write(&empty, end_of_directory).expect("empty zip");
        assert!(first_zip_entry(unzip, &empty).is_err());
        let junk = dir.path().join("junk.zip");
        fs::write(&junk, b"not a zip").expect("junk");
        assert!(first_zip_entry(unzip, &junk).is_err());
        assert!(first_zip_entry(unzip, &dir.path().join("none")).is_err());
        assert!(first_zip_entry(&dir.path().join("no-unzip"), &archive).is_err());
    }

    // Real `gpg --status-fd 1 --verify` output (GnuPG 2.5.20, 2026-11-19).
    // Oracle's signature on mysql-8.4.13-linux-glibc2.28-x86_64-minimal.tar.xz
    // (`fixtures/mysql-8.4.13-...tar.xz.asc`, kept to show where these came
    // from):
    const VALID: &str = include_str!("fixtures/mysql-gpg-valid.status");
    const VALID_GPGV: &str = include_str!("fixtures/mysql-gpgv-valid.status");
    // ... on another file.
    const BADSIG: &str = include_str!("fixtures/gpg-badsig.status");
    // ... checked with Oracle's 2023 key file: the same key, expired on
    // 2025-10-22. gpg still exits 0 and prints VALIDSIG.
    const EXPIRED: &str = include_str!("fixtures/gpg-expired-key-2023.status");
    // A `.asc` that is no signature.
    const NODATA: &str = include_str!("fixtures/gpg-nodata.status");
    // A test key (a certify-only primary and a signing subkey) signed
    // `not the archive\n`: checked with its own key, with Oracle's, and
    // signed by two test keys.
    const TEST_VALID: &str = include_str!("fixtures/gpg-test-key-valid.status");
    const NO_PUBKEY: &str = include_str!("fixtures/gpg-no-pubkey.status");
    const TWO_SIGS: &str = include_str!("fixtures/gpg-two-sigs.status");
    const TEST_PRIMARY: &str = "09A2D74704B49FDFA6B2478171127E3F5D4D4A74";
    const TEST_SUBKEY: &str = "DD2A3FDEC54375977D59A26C329385C34814BA6F";
    // Only the tests that run gpg or md5 read it, and those need Unix.
    #[cfg(unix)]
    const TEST_DATA: &[u8] = b"not the archive\n";

    fn refused(text: &str, fingerprint: &str) -> String {
        format!(
            "{:#}",
            check_status(text, fingerprint).expect_err("must be refused")
        )
    }

    #[test]
    fn oracles_real_signature_is_good() {
        check_status(VALID, ORACLE_FINGERPRINT).expect("gpg");
        check_status(VALID_GPGV, ORACLE_FINGERPRINT).expect("gpgv");
        check_status(&VALID.replace('\n', "\r\n"), ORACLE_FINGERPRINT).expect("CRLF");
    }

    #[test]
    fn every_refusal_says_why() {
        for (text, fingerprint, expected) in [
            (BADSIG, ORACLE_FINGERPRINT, "BADSIG"),
            (EXPIRED, ORACLE_FINGERPRINT, "has expired (EXPKEYSIG)"),
            (NODATA, ORACLE_FINGERPRINT, "no signature"),
            (
                NO_PUBKEY,
                ORACLE_FINGERPRINT,
                "not in the key file (ERRSIG)",
            ),
            (TWO_SIGS, TEST_PRIMARY, "holds 2 signatures"),
            (TEST_VALID, ORACLE_FINGERPRINT, "not by BCA43417"),
            ("", ORACLE_FINGERPRINT, "no signature"),
        ] {
            let message = refused(text, fingerprint);
            assert!(message.contains(expected), "{expected:?}: {message}");
        }
    }

    /// The pinned fingerprint is compared with the primary key's, the last
    /// field, not with the key that made the signature.
    #[test]
    fn the_primary_fingerprint_is_compared() {
        check_status(TEST_VALID, TEST_PRIMARY).expect("signed by the subkey of this primary");
        let message = refused(TEST_VALID, TEST_SUBKEY);
        assert!(
            message.contains(&format!("of the primary key {TEST_PRIMARY}")),
            "{message}"
        );
        // A VALIDSIG whose first field is the pinned key, and whose primary
        // is another: refused.
        let swapped = TEST_VALID.replace(
            &format!("VALIDSIG {TEST_SUBKEY}"),
            &format!("VALIDSIG {ORACLE_FINGERPRINT}"),
        );
        assert!(refused(&swapped, ORACLE_FINGERPRINT).contains("not by"));
    }

    #[test]
    fn a_status_that_proves_less_is_refused() {
        let without = |keyword: &str| {
            VALID
                .lines()
                .filter(|line| !line.contains(keyword))
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        };
        assert!(refused(&without("GOODSIG"), ORACLE_FINGERPRINT).contains("no GOODSIG"));
        assert!(refused(&without("VALIDSIG"), ORACLE_FINGERPRINT).contains("0 VALIDSIG"));
        // gpg 1.x had no primary fingerprint field.
        let short = VALID.replace(&format!(" 00 {ORACLE_FINGERPRINT}"), " 00");
        assert!(refused(&short, ORACLE_FINGERPRINT).contains("not 10"));
        let twice = VALID.replace(
            "[GNUPG:] TRUST",
            &format!(
                "{}\n[GNUPG:] TRUST",
                VALID.lines().nth(4).unwrap_or_default()
            ),
        );
        assert!(refused(&twice, ORACLE_FINGERPRINT).contains("2 VALIDSIG"));
        // A text-mode signature: it would check the file with its line ends
        // normalized.
        let text_mode = VALID.replace(" 1 8 00 BCA4", " 1 8 01 BCA4");
        assert!(refused(&text_mode, ORACLE_FINGERPRINT).contains("class is 01"));
        // An expired subkey that did not sign is not a refusal by itself.
        let old_subkey = VALID.replace("[GNUPG:] TRUST", "[GNUPG:] KEYEXPIRED 1\n[GNUPG:] TRUST");
        check_status(&old_subkey, ORACLE_FINGERPRINT).expect("the primary signed");
        for keyword in ["REVKEYSIG", "KEYREVOKED", "EXPSIG", "FAILURE"] {
            let text = format!("{VALID}[GNUPG:] {keyword} x\n");
            assert!(
                check_status(&text, ORACLE_FINGERPRINT).is_err(),
                "{keyword}"
            );
        }
        // Only machine lines count: a GOODSIG in plain text proves nothing.
        let plain = VALID.replace("[GNUPG:] ", "");
        assert!(refused(&plain, ORACLE_FINGERPRINT).contains("no signature"));
    }

    /// Without gpg the check stops, and says how to get it.
    #[test]
    fn no_gpg_is_an_error_never_a_skip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("gpg").display().to_string();
        let message = format!(
            "{:#}",
            find_gpg_in(&[missing.as_str()]).expect_err("none there")
        );
        assert!(message.contains("install GnuPG"), "{message}");
        let found = find_gpg_in(&[missing.as_str(), env!("CARGO_MANIFEST_DIR")]);
        assert!(found.is_err(), "a folder is not a program");

        let none = Path::new(&missing);
        let any = dir.path();
        let home = dir.path().join("home");
        let err = verify(none, any, any, any, &home, ORACLE_FINGERPRINT).expect_err("no gpg");
        assert!(format!("{err:#}").contains("cannot run gpg"), "{err:#}");
        assert!(!home.exists());
    }

    #[test]
    fn md5_output_is_32_lowercase_hex() {
        let md5 = parse_md5_output("fb536f5d26e3e43e933aec34679c0740\n").expect("valid");
        assert_eq!(md5[0], 0xfb);
        for text in [
            "",
            "fb536f5d26e3e43e933aec34679c074\n",
            "FB536F5D26E3E43E933AEC34679C0740\n",
            "fb536f5d26e3e43e933aec34679c0740  file\n",
            "fb536f5d26e3e43e933aec34679c0740\n\n",
        ] {
            assert_eq!(parse_md5_output(text), None, "{text:?}");
        }
    }

    /// The real `md5` on a Mac.
    #[cfg(target_os = "macos")]
    #[test]
    fn md5_of_a_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("data");
        fs::write(&path, TEST_DATA).expect("write");
        let md5 = md5_of(&path).expect("md5");
        // `printf 'not the archive\n' | openssl md5` gives the same.
        assert_eq!(
            super::super::upstream::hex(&md5),
            "88fbe7e92216ef7161ef852d6afc6a4a"
        );
    }

    /// The real gpg, where the CI runners have it (macOS and Linux), with
    /// the test key: a good signature, the wrong pinned key, changed data,
    /// and a key file without the signer. Never the owner's keyring.
    #[cfg(unix)]
    #[test]
    fn real_gpg_checks_a_test_signature() {
        let gpg = find_gpg().expect("the macOS and Linux runners have gpg");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/manifest/fixtures");
        let key = fixtures.join("gpg-test-key.asc");
        let signature = fixtures.join("gpg-test-signature.asc");
        let dir = tempfile::tempdir().expect("tempdir");
        let data = dir.path().join("data");
        fs::write(&data, TEST_DATA).expect("write");
        let home = dir.path().join("home");
        let check = |key: &Path, data: &Path, fingerprint: &str| {
            verify(&gpg, key, &signature, data, &home, fingerprint)
        };

        check(&key, &data, TEST_PRIMARY).expect("good");
        assert!(!home.exists(), "the home folder is deleted");

        let message = format!(
            "{:#}",
            check(&key, &data, ORACLE_FINGERPRINT).expect_err("pinned")
        );
        assert!(message.contains("not by BCA43417"), "{message}");

        let changed = dir.path().join("changed");
        fs::write(&changed, b"not the archive!\n").expect("write");
        let message = format!(
            "{:#}",
            check(&key, &changed, TEST_PRIMARY).expect_err("bad")
        );
        assert!(message.contains("BADSIG"), "{message}");

        let oracle = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(ORACLE_KEY_FILE);
        let message = format!(
            "{:#}",
            check(&oracle, &data, TEST_PRIMARY).expect_err("unknown")
        );
        assert!(message.contains("ERRSIG"), "{message}");
    }
}
