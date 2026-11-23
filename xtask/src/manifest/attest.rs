//! GitHub's build attestation for a theseus-rs PostgreSQL archive (ADR 0023,
//! section 3). A `.sha256` file comes from the place that serves the archive,
//! so it only shows that the download is not damaged. The attestation is a
//! Sigstore signature that ties the archive's sha256 to the workflow that
//! built it, in the `theseus-rs/postgresql-binaries` repository, on a GitHub
//! runner. It does not show that PostgreSQL's own source is sound.
//!
//! `gh attestation verify` does the Sigstore part (certificate chain,
//! transparency log, the digest of the file). [`check_result`] then reads the
//! JSON it prints and demands the exact identity again, so the check does not
//! rest on the flags of one command alone.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use semver::Version;
use serde_json::Value;

use super::upstream::hex;

/// The repository that builds the archives. The only one accepted.
const REPO: &str = "theseus-rs/postgresql-binaries";

/// The workflow that signs the attestations: a reusable workflow, so it is
/// the signer. Pinned by path; the tag it ran at must be the archive's own
/// version. If theseus-rs renames it, the build stops until this changes.
const SIGNER_WORKFLOW: &str = ".github/workflows/build.yml";

/// The only OIDC issuer of a GitHub Actions certificate.
const ISSUER: &str = "https://token.actions.githubusercontent.com";

/// The predicate type of a GitHub build attestation.
const PREDICATE_TYPE: &str = "https://slsa.dev/provenance/v1";

/// Where `gh` may be. Never searched in `PATH`.
const GH_PATHS: [&str; 3] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"];

/// The `gh` program, from [`GH_PATHS`].
///
/// # Errors
///
/// When none is there: the check is never skipped.
fn find_gh() -> Result<PathBuf> {
    find_gh_in(&GH_PATHS)
}

/// The first of `paths` that is a file.
fn find_gh_in(paths: &[&str]) -> Result<PathBuf> {
    paths
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "no gh program at {}: install the GitHub CLI (`brew install gh`) and run \
                 `gh auth login`. The attestation of a PostgreSQL archive is never skipped",
                paths.join(", ")
            )
        })
}

/// `gh attestation verify` on `file`, with the `gh` of [`find_gh`].
///
/// # Errors
///
/// No `gh`, or [`run_gh`] fails.
pub(super) fn verify_with_gh(file: &Path, version: &Version) -> Result<String> {
    run_gh(&find_gh()?, file, version)
}

/// The tag the archive of `version` is built at.
fn tag_ref(version: &Version) -> String {
    format!("refs/tags/{version}")
}

/// Runs `gh attestation verify` on `file` and returns its JSON. The identity
/// is pinned: the repository, the signer workflow, the tag, and GitHub's own
/// runners. `gh` exits with an error when there is no attestation, or none
/// that fits.
///
/// # Errors
///
/// `gh` cannot run, or it refused the file (with its own message).
fn run_gh(gh: &Path, file: &Path, version: &Version) -> Result<String> {
    let mut cmd = Command::new(gh);
    cmd.args(["attestation", "verify"])
        .arg(file)
        .args(["--hostname", "github.com", "--repo", REPO])
        .arg("--signer-workflow")
        .arg(format!("{REPO}/{SIGNER_WORKFLOW}"))
        .arg("--source-ref")
        .arg(tag_ref(version))
        .args(["--deny-self-hosted-runners", "--format", "json"])
        .stdin(Stdio::null())
        .env("GH_PROMPT_DISABLED", "1");
    let out = cmd
        .output()
        .with_context(|| format!("cannot run {}", gh.display()))?;
    ensure!(
        out.status.success(),
        "`gh attestation verify` refused {} ({}): {}",
        file.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8(out.stdout).context("`gh attestation verify` printed text that is not UTF-8")
}

/// Accepts the JSON of `gh attestation verify --format json` only when at
/// least one verified attestation says all of this:
///
/// - it is a GitHub build attestation of the SLSA provenance type;
/// - its certificate is GitHub Actions' (the issuer), for the repository
///   [`REPO`], and its signer is [`SIGNER_WORKFLOW`] at the tag of `version`;
/// - it was built on a GitHub-hosted runner;
/// - it names the archive `name` with exactly `sha256`.
///
/// Only the certificate holds values that the workflow cannot forge, so the
/// identity is read from there and not from the predicate.
///
/// # Errors
///
/// Why no attestation fits: with several, the first one's reason.
pub(super) fn check_result(
    json: &str,
    name: &str,
    sha256: &[u8; 32],
    version: &Version,
) -> Result<()> {
    let value: Value = serde_json::from_str(json).context("the attestation is not JSON")?;
    let entries = value
        .as_array()
        .context("the attestation is not a JSON array")?;
    let mut first = None;
    for entry in entries {
        match check_entry(entry, name, sha256, version) {
            Ok(()) => return Ok(()),
            Err(err) => {
                first.get_or_insert(err);
            }
        }
    }
    match first {
        Some(err) => Err(err.context(format!(
            "no attestation of {REPO} fits {name} at {}",
            tag_ref(version)
        ))),
        None => bail!("{name} has no verified attestation"),
    }
}

fn check_entry(entry: &Value, name: &str, sha256: &[u8; 32], version: &Version) -> Result<()> {
    let result = &entry["verificationResult"];
    let cert = &result["signature"]["certificate"];
    let text = |value: &Value, what: &str| -> Result<String> {
        value
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("the attestation has no {what}"))
    };
    let expect = |what: &str, got: String, want: &str| -> Result<()> {
        ensure!(got == want, "its {what} is {got:?}, not {want:?}");
        Ok(())
    };

    let tag = tag_ref(version);
    expect(
        "predicate type",
        text(&result["statement"]["predicateType"], "predicate type")?,
        PREDICATE_TYPE,
    )?;
    expect(
        "certificate issuer",
        text(&cert["issuer"], "certificate issuer")?,
        ISSUER,
    )?;
    expect(
        "repository",
        text(&cert["githubWorkflowRepository"], "repository")?,
        REPO,
    )?;
    expect(
        "source repository",
        text(&cert["sourceRepositoryURI"], "source repository")?,
        &format!("https://github.com/{REPO}"),
    )?;
    expect(
        "signer workflow",
        text(&cert["subjectAlternativeName"], "signer workflow")?,
        &format!("https://github.com/{REPO}/{SIGNER_WORKFLOW}@{tag}"),
    )?;
    expect(
        "source ref",
        text(&cert["sourceRepositoryRef"], "source ref")?,
        &tag,
    )?;
    expect(
        "runner",
        text(&cert["runnerEnvironment"], "runner")?,
        "github-hosted",
    )?;

    let want = hex(sha256);
    let subjects = result["statement"]["subject"]
        .as_array()
        .context("the attestation has no subject")?;
    ensure!(
        subjects
            .iter()
            .any(|s| s["name"] == name && s["digest"]["sha256"] == want.as_str()),
        "it does not name {name} with sha256 {want}"
    );
    Ok(())
}

/// What the tests need from a real capture, and a way to make the refusals.
#[cfg(test)]
pub(super) mod testing {
    use serde_json::Value;

    use super::*;

    /// A real `gh attestation verify --format json` of
    /// `postgresql-18.7.0-aarch64-apple-darwin.tar.gz`, taken with `--repo`
    /// alone (its `verifiedIdentity` shows that): the pinned flags only
    /// narrow what gh accepts, and `check_result` reads the certificate, which
    /// is the same. The bundle itself is cut out.
    pub(in crate::manifest) const REAL: &str =
        include_str!("fixtures/attestation-postgresql-18.7.0-aarch64-apple-darwin.json");

    /// The real capture, made to say that `name` has `sha256` and was built
    /// at the tag of `version`.
    pub(in crate::manifest) fn sample(name: &str, sha256: &[u8; 32], version: &Version) -> String {
        let mut value: Value = serde_json::from_str(REAL).expect("fixture");
        let result = &mut value[0]["verificationResult"];
        let tag = tag_ref(version);
        let cert = &mut result["signature"]["certificate"];
        cert["subjectAlternativeName"] =
            format!("https://github.com/{REPO}/{SIGNER_WORKFLOW}@{tag}").into();
        cert["sourceRepositoryRef"] = tag.into();
        result["statement"]["subject"][0]["name"] = name.into();
        result["statement"]["subject"][0]["digest"]["sha256"] = hex(sha256).into();
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{REAL, sample};
    use super::*;

    const NAME: &str = "postgresql-18.7.0-aarch64-apple-darwin.tar.gz";
    const SHA: &str = "86456c0ed99d2466524eaa074db6580cf7d748b91042e252c83eaca9a9d216e7";

    fn version() -> Version {
        Version::new(18, 7, 0)
    }

    fn sha() -> [u8; 32] {
        super::super::upstream::parse_hex(SHA).expect("hex")
    }

    /// The real capture with one value changed at `path` (`/0/...`).
    fn with(path: &str, new: Value) -> String {
        let mut value: Value = serde_json::from_str(REAL).expect("fixture");
        *value.pointer_mut(path).expect("path") = new;
        value.to_string()
    }

    fn refused(json: &str) -> String {
        format!(
            "{:#}",
            check_result(json, NAME, &sha(), &version()).expect_err("refused")
        )
    }

    #[test]
    fn the_real_attestation_of_a_theseus_archive_is_accepted() {
        check_result(REAL, NAME, &sha(), &version()).expect("accepted");
    }

    #[test]
    fn a_made_up_sample_is_accepted_for_its_own_name_and_version() {
        let version = Version::new(17, 12, 0);
        let json = sample(
            "postgresql-17.12.0-x86_64-pc-windows-msvc.zip",
            &[7; 32],
            &version,
        );
        check_result(
            &json,
            "postgresql-17.12.0-x86_64-pc-windows-msvc.zip",
            &[7; 32],
            &version,
        )
        .expect("accepted");
    }

    #[test]
    fn no_attestation_is_refused() {
        assert!(refused("[]").contains("has no verified attestation"));
        assert!(refused("{}").contains("not a JSON array"));
        assert!(refused("not json").contains("not JSON"));
    }

    #[test]
    fn another_digest_is_refused() {
        let err = format!(
            "{:#}",
            check_result(REAL, NAME, &[1; 32], &version()).expect_err("refused")
        );
        assert!(err.contains("does not name"), "{err}");
    }

    #[test]
    fn another_file_name_is_refused() {
        let err = format!(
            "{:#}",
            check_result(
                REAL,
                "postgresql-18.7.0-x86_64-apple-darwin.tar.gz",
                &sha(),
                &version()
            )
            .expect_err("refused")
        );
        assert!(err.contains("does not name"), "{err}");
    }

    #[test]
    fn another_repository_workflow_or_runner_is_refused() {
        let cert = "/0/verificationResult/signature/certificate";
        for (field, bad, expect) in [
            (
                "githubWorkflowRepository",
                "evil/postgresql-binaries",
                "repository",
            ),
            (
                "sourceRepositoryURI",
                "https://github.com/evil/postgresql-binaries",
                "source repository",
            ),
            (
                "subjectAlternativeName",
                "https://github.com/theseus-rs/postgresql-binaries/.github/workflows/other.yml@refs/tags/18.7.0",
                "signer workflow",
            ),
            ("runnerEnvironment", "self-hosted", "runner"),
            ("issuer", "https://example.org", "certificate issuer"),
        ] {
            let json = with(&format!("{cert}/{field}"), bad.into());
            let err = refused(&json);
            assert!(err.contains(&format!("its {expect} is")), "{field}: {err}");
        }
    }

    #[test]
    fn a_tag_of_another_version_is_refused() {
        let cert = "/0/verificationResult/signature/certificate";
        let json = with(
            &format!("{cert}/sourceRepositoryRef"),
            "refs/tags/17.12.0".into(),
        );
        assert!(refused(&json).contains("its source ref is"));
        let json = with(
            &format!("{cert}/subjectAlternativeName"),
            "https://github.com/theseus-rs/postgresql-binaries/.github/workflows/build.yml@refs/tags/17.12.0"
                .into(),
        );
        assert!(refused(&json).contains("its signer workflow is"));
        // A branch is not a tag.
        let json = with(
            &format!("{cert}/sourceRepositoryRef"),
            "refs/heads/main".into(),
        );
        assert!(refused(&json).contains("its source ref is"));
    }

    #[test]
    fn another_predicate_type_is_refused() {
        let json = with(
            "/0/verificationResult/statement/predicateType",
            "https://example.org/other".into(),
        );
        assert!(refused(&json).contains("its predicate type is"));
    }

    #[test]
    fn a_missing_field_is_refused_and_never_assumed() {
        let mut value: Value = serde_json::from_str(REAL).expect("fixture");
        value[0]["verificationResult"]["signature"]["certificate"]
            .as_object_mut()
            .expect("object")
            .remove("runnerEnvironment");
        let err = refused(&value.to_string());
        assert!(err.contains("has no runner"), "{err}");
    }

    /// The statement names three files. The archive's name with another
    /// file's digest is not an attestation of the archive.
    #[test]
    fn the_name_and_the_digest_must_be_of_the_same_subject() {
        let value: Value = serde_json::from_str(REAL).expect("fixture");
        let subjects = &value[0]["verificationResult"]["statement"]["subject"];
        let other = subjects[2]["digest"]["sha256"].as_str().expect("digest");
        let err = format!(
            "{:#}",
            check_result(
                REAL,
                NAME,
                &super::super::upstream::parse_hex(other).expect("hex"),
                &version()
            )
            .expect_err("refused")
        );
        assert!(err.contains("does not name"), "{err}");
    }

    /// `gh`'s own `--signer-workflow` match is a prefix match, so these
    /// near misses are stopped here, by equality.
    #[test]
    fn the_signer_and_the_tag_are_compared_exactly() {
        let cert = "/0/verificationResult/signature/certificate";
        let signer =
            "https://github.com/theseus-rs/postgresql-binaries/.github/workflows/build.yml";
        for san in [
            format!("{signer}.evil.yml@refs/tags/18.7.0"),
            format!("{signer}@refs/tags/18.7.0-x"),
            format!("{signer}@refs/tags/18.7.0.1"),
            format!("{signer}/@refs/tags/18.7.0"),
        ] {
            let json = with(
                &format!("{cert}/subjectAlternativeName"),
                san.as_str().into(),
            );
            assert!(refused(&json).contains("its signer workflow is"), "{san}");
        }
        let json = with(
            &format!("{cert}/sourceRepositoryRef"),
            "refs/tags/18.7.0.1".into(),
        );
        assert!(refused(&json).contains("its source ref is"));
    }

    #[test]
    fn a_field_of_the_wrong_type_is_refused() {
        let cert = "/0/verificationResult/signature/certificate";
        for bad in [Value::Null, 7.into(), true.into(), Value::Array(vec![])] {
            let json = with(&format!("{cert}/runnerEnvironment"), bad);
            assert!(refused(&json).contains("has no runner"));
        }
        let json = with("/0/verificationResult/statement/subject", "x".into());
        assert!(refused(&json).contains("has no subject"));
        let json = with(
            "/0/verificationResult/statement/subject/0/digest/sha256",
            7.into(),
        );
        assert!(refused(&json).contains("does not name"));
    }

    #[test]
    fn gh_is_looked_for_by_absolute_path_only() {
        let message = format!(
            "{:#}",
            find_gh_in(&["/nonexistent/one/gh", "/nonexistent/two/gh"]).expect_err("no gh")
        );
        assert!(
            message.contains("/nonexistent/one/gh, /nonexistent/two/gh"),
            "{message}"
        );
        assert!(message.contains("never skipped"), "{message}");
        let dir = tempfile::tempdir().expect("tempdir");
        let here = dir.path().join("gh");
        std::fs::write(&here, "").expect("write");
        let found = find_gh_in(&["/nonexistent/one/gh", here.to_str().expect("utf-8")])
            .expect("the second");
        assert_eq!(found, here);
    }

    // Unix only: the fake `gh` is a shell script with mode bits.
    #[cfg(unix)]
    fn fake_gh(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("gh");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// A `gh` that refuses is an error with its own words; one that prints is
    /// believed only by `check_result`. The flags are the pinned ones.
    // Unix only: it runs the shell script of `fake_gh`.
    #[cfg(unix)]
    #[test]
    fn a_gh_that_refuses_or_is_missing_is_an_error_never_a_skip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("archive");
        let refusing = fake_gh(dir.path(), "echo 'HTTP 401: bad credentials' >&2; exit 1");
        let err = format!(
            "{:#}",
            run_gh(&refusing, &file, &version()).expect_err("refused")
        );
        assert!(err.contains("refused") && err.contains("HTTP 401"), "{err}");

        let missing = dir.path().join("nothing-here");
        let err = format!(
            "{:#}",
            run_gh(&missing, &file, &version()).expect_err("no program")
        );
        assert!(err.contains("cannot run"), "{err}");

        let args = dir.path().join("args");
        let good = fake_gh(
            dir.path(),
            &format!("echo \"$@\" > '{}'; echo '[]'", args.display()),
        );
        assert_eq!(run_gh(&good, &file, &version()).expect("printed"), "[]\n");
        let args = std::fs::read_to_string(&args).expect("args");
        for flag in [
            "attestation verify",
            "--hostname github.com",
            "--repo theseus-rs/postgresql-binaries",
            "--signer-workflow theseus-rs/postgresql-binaries/.github/workflows/build.yml",
            "--source-ref refs/tags/18.7.0",
            "--deny-self-hosted-runners",
            "--format json",
        ] {
            assert!(args.contains(flag), "{flag}: {args}");
        }
    }

    #[test]
    fn one_good_attestation_among_bad_ones_is_enough() {
        let mut value: Value = serde_json::from_str(REAL).expect("fixture");
        let mut bad = value[0].clone();
        bad["verificationResult"]["signature"]["certificate"]["runnerEnvironment"] =
            "self-hosted".into();
        value.as_array_mut().expect("array").insert(0, bad);
        check_result(&value.to_string(), NAME, &sha(), &version()).expect("one fits");
    }
}
