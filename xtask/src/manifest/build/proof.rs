//! The proof each file needs before its sha256 goes in the manifest, and
//! Oracle's whole proof for the MySQL archive.

use std::fs::{self};
use std::io::{self};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::fetch::{Fetch, fetch_bytes};
use marsa_core::runtimes::Runtime;

use super::super::upstream::{
    Algorithm, Layout, MYSQL_FOLDERS, Source, SumFile, hex, sum_file_for,
};
use super::download::{hash_file, temp_download};

/// The upstream proof a file needs before its sha256 goes in the manifest.
/// The database servers of M6 each need the proof of their own source (ADR
/// 0023, section 3); without it the build stops. An `Md5` sum is never
/// accepted alone: only [`Proof::OracleSignature`] takes one, together with
/// Oracle's GPG signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Proof {
    /// The upstream sum if the source publishes one `build` knows. Without
    /// one, only a warning: today, Windows PHP in `releases/archives/`
    /// (windows.php.net lists only sha1 there). PHP, Node, Composer, Caddy.
    SumIfPublished,
    /// The theseus-rs `<archive>.sha256` ([`Layout::Single`]) and no other
    /// source, of the recipe's own URL. PostgreSQL.
    TheseusSha256,
    /// Oracle's GPG signature `<archive>.asc` by the pinned key, and its
    /// `<archive>.md5`, both ([`check_oracle`]). The MySQL archive of the
    /// recipe's own URL, or the same file name in Oracle's other folder
    /// (`archives/`, for a version that is not current any more).
    OracleSignature,
    /// The `sha256sums.txt` of one of our own `libaio-<v>-r<n>` releases,
    /// which MySQL's recipe names for `libaio.so.1` and `libnuma.so.1`. No
    /// upstream signs them: the trust is in our build and in the owner's
    /// signature on the manifest.
    OurLibaio,
    /// The `sha256sums.txt` of one of our own `valkey-<v>-r<n>` releases
    /// (ADR 0023, section 3): the archive the Valkey recipe names, and no
    /// other. The build workflow checked the source tarball against
    /// `valkey-io/valkey-hashes`; the trust is in that build and in the
    /// owner's signature on the manifest.
    OurValkey,
    /// The sha256 `digest` GitHub's API reports for the asset of a Mailpit
    /// release ([`Layout::GithubDigest`]): the archive the recipe names, and no
    /// other. Mailpit publishes no sum file and no attestation, so this proves
    /// only that the download is whole; the trust is in the owner's signature
    /// on the manifest, as for theseus-rs.
    MailpitDigest,
}

impl Proof {
    /// An exhaustive `match`, so each new runtime has to choose. MySQL's
    /// `archive` is Oracle's; its other files are our own builds. Valkey is
    /// our own build.
    pub(super) fn of(runtime: Runtime, artifact: &str) -> Proof {
        match runtime {
            Runtime::Php | Runtime::Node | Runtime::Composer | Runtime::Caddy => {
                Proof::SumIfPublished
            }
            Runtime::Postgres => Proof::TheseusSha256,
            Runtime::Mysql if artifact == "archive" => Proof::OracleSignature,
            Runtime::Mysql => Proof::OurLibaio,
            Runtime::Valkey => Proof::OurValkey,
            Runtime::Mailpit => Proof::MailpitDigest,
        }
    }

    /// Whether the file at `url` can give this proof, before anything is
    /// downloaded. `sums` is the sum file [`sum_file_for`] finds for `url`
    /// (`None`: no known one), and `template` the recipe's URL for the file.
    /// The sum is compared after the download.
    pub(super) fn allows(
        self,
        url: &str,
        template: Option<&str>,
        sums: Option<&SumFile>,
    ) -> Result<()> {
        match (self, sums) {
            (Proof::SumIfPublished | Proof::TheseusSha256, Some(sums))
                if sums.algorithm == Algorithm::Md5 =>
            {
                bail!(
                    "its only upstream sum is the MD5 {}, and an MD5 is never accepted alone",
                    sums.url
                )
            }
            (Proof::SumIfPublished, _) => Ok(()),
            // An override could name another theseus-rs archive, of another
            // version or target, with its own matching `.sha256`.
            (Proof::TheseusSha256, _) if template != Some(url) => bail!(
                "it is not the recipe's URL {}, and only that theseus-rs archive is accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::TheseusSha256, Some(sums))
                if sums.source == Source::Theseus
                    && sums.layout == Layout::Single
                    && sums.algorithm == Algorithm::Sha256 =>
            {
                Ok(())
            }
            (Proof::TheseusSha256, Some(sums)) => bail!(
                "its sum would come from {}, but only the theseus-rs `.sha256` is accepted",
                sums.url
            ),
            (Proof::TheseusSha256, None) => bail!(
                "no upstream sum that `manifest build` knows, and only the theseus-rs \
                 `.sha256` is accepted"
            ),
            // Another signed Oracle archive, such as an older MySQL with
            // known holes, would pass the signature check too.
            (Proof::OracleSignature, _) if !is_recipes_archive(url, template) => bail!(
                "it is not the recipe's URL {}, and only that Oracle archive is accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::OracleSignature, Some(sums))
                if sums.source == Source::Oracle
                    && sums.layout == Layout::Single
                    && sums.algorithm == Algorithm::Md5 =>
            {
                Ok(())
            }
            (Proof::OracleSignature, _) => bail!(
                "only an Oracle MySQL 8.4 archive on cdn.mysql.com, with its `.md5` and its \
                 signature `.asc`, is accepted"
            ),
            // A file of another release, or of another architecture, would
            // carry its own matching sum: so the file is the recipe's.
            (Proof::OurLibaio, _) if template != Some(url) => bail!(
                "it is not the recipe's URL {}, and only that file of our libaio release is \
                 accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::OurLibaio, Some(sums))
                if sums.source == Source::OurLibaio
                    && sums.layout == Layout::List
                    && sums.algorithm == Algorithm::Sha256 =>
            {
                Ok(())
            }
            (Proof::OurLibaio, _) => bail!(
                "only a file of our own libaio release, with its `sha256sums.txt`, is accepted"
            ),
            // An override could name another of our Valkey archives, of
            // another version, revision, or platform, with its own matching
            // sum: so the file is the recipe's. A rebuild changes the recipe.
            (Proof::OurValkey, _) if template != Some(url) => bail!(
                "it is not the recipe's URL {}, and only that archive of our Valkey release is \
                 accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::OurValkey, Some(sums))
                if sums.source == Source::OurValkey
                    && sums.layout == Layout::List
                    && sums.algorithm == Algorithm::Sha256 =>
            {
                Ok(())
            }
            (Proof::OurValkey, _) => bail!(
                "only an archive of our own Valkey release, with its `sha256sums.txt`, is \
                 accepted"
            ),
            // An override could name another Mailpit archive, of another
            // version or platform, with its own matching digest.
            (Proof::MailpitDigest, _) if template != Some(url) => bail!(
                "it is not the recipe's URL {}, and only that Mailpit archive is accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::MailpitDigest, Some(sums))
                if sums.source == Source::Mailpit
                    && sums.layout == Layout::GithubDigest
                    && sums.algorithm == Algorithm::Sha256 =>
            {
                Ok(())
            }
            (Proof::MailpitDigest, _) => bail!(
                "only a Mailpit release archive, with the sha256 digest of GitHub's release \
                 API, is accepted"
            ),
        }
    }

    /// Whether the file is kept in the cache after the build: only when a
    /// later check reads it (PHP, the sha512 of Caddy). A file this proof
    /// checks by its sha256 alone is hashed on the way and never written,
    /// and a MySQL archive is deleted after its checks.
    pub(super) fn keeps_file(self) -> bool {
        match self {
            Proof::SumIfPublished => true,
            Proof::TheseusSha256
            | Proof::OracleSignature
            | Proof::OurLibaio
            | Proof::OurValkey
            | Proof::MailpitDigest => false,
        }
    }
}

/// Whether `url` is the recipe's own archive `template`, or the file of the
/// same name in another folder of Oracle's (a version that is not current
/// any more moves from `Downloads/` to `archives/`). The name carries the
/// version and the platform, so this never accepts another archive.
pub(super) fn is_recipes_archive(url: &str, template: Option<&str>) -> bool {
    let Some(template) = template else {
        return false;
    };
    if url == template {
        return true;
    }
    let (Some((dir, name)), Some((template_dir, template_name))) =
        (url.rsplit_once('/'), template.rsplit_once('/'))
    else {
        return false;
    };
    name == template_name && MYSQL_FOLDERS.contains(&dir) && MYSQL_FOLDERS.contains(&template_dir)
}

/// The programs behind Oracle's proof: `oracle::System` in the build, a
/// stub in tests.
pub(in crate::manifest) trait OracleCheck {
    /// Checks that `signature`, the `.asc`, is Oracle's good signature on
    /// `file`, by the pinned key.
    fn signature(&self, signature: &Path, file: &Path) -> Result<()>;
    /// The MD5 of `file`.
    fn md5(&self, file: &Path) -> Result<[u8; 16]>;
    /// The name of the first entry of the archive `file`, of the `kind` its
    /// name gives, as it is written there (`mysql-8.4.13-.../bin/mysqld`).
    fn first_entry(&self, file: &Path, kind: ArchiveKind) -> Result<String>;
}

/// The two kinds of Oracle's MySQL archives: a tar archive (`.tar.gz`,
/// `.tar.xz`) for macOS and Linux, a `.zip` for Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::manifest) enum ArchiveKind {
    Tar,
    Zip,
}

/// The top folder of a MySQL archive called `name`: the name without its
/// `.tar.gz`, `.tar.xz`, or `.zip`, and the kind of archive. Oracle's
/// signature does not bind the archive to its name, so a hijacked CDN could
/// serve an older signed archive under a new name: its top folder would
/// still say the old version.
pub(super) fn top_folder_of(name: &str) -> Result<(&str, ArchiveKind)> {
    let tar = name
        .strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".tar.xz"));
    if let Some(top) = tar {
        return Ok((top, ArchiveKind::Tar));
    }
    let top = name
        .strip_suffix(".zip")
        .with_context(|| format!("{name} is not a .tar.gz, .tar.xz, or .zip archive"))?;
    Ok((top, ArchiveKind::Zip))
}

/// A file whose proof passed: what the manifest records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::manifest) struct Checked {
    pub url: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

/// Oracle's whole proof for the MySQL archive at `url`
/// ([`Proof::OracleSignature`]), with `url` as the recipe's own URL:
///
/// 1. `url` is an Oracle MySQL 8.4 archive, checked before any download;
/// 2. its `.md5` and its signature `.asc` are fetched first, so without
///    either nothing large is downloaded;
/// 3. the archive is downloaded to `work` and hashed on the way;
/// 4. the signature is good and by the pinned key, then its MD5 matches,
///    then its sha256 is still the one hashed on the way, so all three are
///    of the same bytes;
/// 5. the archive's top folder is the one its file name gives, so a signed
///    archive of another version is not accepted under this version's name.
///
/// The archive and its `.asc` are deleted after, whatever the result: they
/// are 80 to 834 MB.
///
/// # Errors
///
/// The first step that fails, with the URL.
pub(in crate::manifest) async fn check_oracle(
    fetch: &impl Fetch,
    url: &str,
    work: &Path,
    check: &impl OracleCheck,
) -> Result<Checked> {
    let sums = sum_file_for(url);
    // Any file of Oracle's two folders passes here: this function does not
    // know the recipe's URL. `build` checks the URL against the recipe's
    // before it calls this; the `check-mysql` command is for trying one file.
    Proof::OracleSignature
        .allows(url, Some(url), sums.as_ref())
        .with_context(|| url.to_owned())?;
    let sums = sums.with_context(|| format!("{url} has no Oracle `.md5`"))?;
    let md5_text = fetch_bytes(fetch, &sums.url, 4096)
        .await
        .with_context(|| format!("{url}: Oracle's md5"))?;
    let md5_text =
        String::from_utf8(md5_text).with_context(|| format!("{} is not text", sums.url))?;
    let expected = sums.find(&md5_text).with_context(|| sums.url.clone())?;
    let signature_url = format!("{url}.asc");
    let signature = fetch_bytes(fetch, &signature_url, 64 * 1024)
        .await
        .with_context(|| {
            format!("{url}: Oracle's signature, without which an MD5 is never accepted")
        })?;

    fs::create_dir_all(work).with_context(|| format!("cannot create {}", work.display()))?;
    // Fixed names, so a run that was killed leaves nothing the next one
    // does not overwrite or delete.
    let path = work.join("archive");
    let asc = work.join("archive.asc");
    let remove = || {
        for leftover in [&path, &asc] {
            if let Err(err) = fs::remove_file(leftover)
                && err.kind() != io::ErrorKind::NotFound
            {
                println!("    warning: cannot delete {}: {err}", leftover.display());
            }
        }
    };
    remove();
    let checked = async {
        fs::write(&asc, &signature).with_context(|| format!("cannot write {}", asc.display()))?;
        let file = temp_download(fetch, &[url.to_owned()], &path).await?;
        check
            .signature(&asc, &path)
            .context("Oracle's signature check failed")?;
        let md5 = check.md5(&path)?;
        ensure!(
            md5.as_slice() == expected.as_slice(),
            "md5 is {}, but {} says {}",
            hex(&md5),
            sums.url,
            hex(&expected)
        );
        let (now, _) = hash_file(&path)?;
        ensure!(
            now == file.sha256,
            "{} changed on disk during its checks",
            path.display()
        );
        let name = url.rsplit_once('/').map_or(url, |(_, name)| name);
        let (want, kind) = top_folder_of(name)?;
        let first = check.first_entry(&path, kind)?;
        let top = first.split('/').next().unwrap_or_default();
        ensure!(
            top == want,
            "the archive's first entry is in {top:?}, not in {want:?}, the folder its name gives"
        );
        Ok(Checked {
            url: file.url,
            sha256: file.sha256,
            size: file.size,
        })
    }
    .await;
    remove();
    checked.with_context(|| url.to_owned())
}
