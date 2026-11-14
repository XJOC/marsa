//! `marsa-helper`: the privileged helper.
//!
//! It accepts only typed JSON requests from a closed list of admin operations
//! and never runs a command string it receives. It does not depend on
//! `marsa-core` (architecture.md section 7).
//!
//! ```text
//! marsa-helper apply <json>   run a request, print the response
//! marsa-helper pf-load        boot job: load the pf redirect (task M4-08)
//! marsa-helper version        print the version
//! marsa-helper service <SID> <hash>   Windows: the service (task M4-19f)
//! marsa-helper install <SID> <hash>   Windows, elevated: install it (M4-19j)
//! marsa-helper uninstall              Windows, elevated: remove it all (M4-19j)
//! ```
//!
//! The request is one argument, not stdin: osascript's `do shell script` has
//! no stdin, and `quoted form of` keeps the argument whole. Exit codes: `0`
//! every op succeeded, `1` an op failed, `2` a rejected request or a bad
//! command line.

mod install;
mod ops;
mod os;
mod request;
mod response;
mod service;
mod system;
// The helper's Win32 calls (ADR 0018). Windows only: nothing else has them.
#[cfg(windows)]
mod windows;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use crate::ops::pf;
use crate::os::Os;
use crate::request::Request;
use crate::response::{OpResult, Response, code};
use crate::system::{RealSystem, System};

const USAGE: &str = "usage: marsa-helper apply <json> | pf-load | version | service <user SID> <CA SHA-256 | -> | install <user SID> <CA SHA-256 | -> | uninstall";

fn main() -> ExitCode {
    // `args_os`: `args` panics on an argument that is not UTF-8.
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    // The boot job really waits only here. Tests pass their own `sleep`.
    let (line, exit) = cli(&args, Os::current(), &RealSystem, thread::sleep);
    // stdout is the helper's protocol: this one line is all it prints there.
    // A closed stdout is ignored, so the exit code still tells what happened.
    //
    // `apply` must never write to stderr, not one byte. On exit 1 or 2,
    // osascript's `do shell script` raises an error whose text is the shell's
    // stderr if it has any, and its stdout only if not. marsad reads this
    // JSON line from that error text (`daemon/setup_api.rs`, `read_answer`),
    // so anything on stderr would hide every result.
    let _ = writeln!(io::stdout().lock(), "{line}");
    ExitCode::from(exit)
}

/// Runs one command line. Returns the line for stdout and the exit code.
/// `sleep` is how the boot job (`pf-load`) waits between its checks.
fn cli(args: &[OsString], os: Os, system: &impl System, sleep: impl Fn(Duration)) -> (String, u8) {
    let response = match args {
        [cmd] if cmd == "version" => return (env!("CARGO_PKG_VERSION").to_owned(), 0),
        [cmd] if cmd == pf::COMMAND => Response::results(vec![pf_load(os, system, sleep)]),
        // Windows: the SCM starts the helper this way (ADR 0018).
        [cmd, rest @ ..] if cmd == service::COMMAND => service::run(rest),
        // Windows, elevated through UAC by marsad (ADR 0018).
        [cmd, rest @ ..] if cmd == install::INSTALL => install::install(rest),
        [cmd] if cmd == install::UNINSTALL => install::uninstall(),
        [cmd, raw] if cmd == "apply" => match Request::from_arg(raw) {
            Ok(request) => Response::results(ops::apply(&request, os, system)),
            Err(err) => Response::error(err.code(), err.to_string()),
        },
        _ => Response::error(code::USAGE, USAGE.to_owned()),
    };
    (response.to_json(), response.exit_code())
}

/// The boot job. Like the pf ops, it runs on macOS only.
fn pf_load(os: Os, system: &impl System, sleep: impl Fn(Duration)) -> OpResult {
    if os == Os::Macos {
        return pf::boot(system, sleep);
    }
    let message = format!("{} is not supported on {}", pf::LOAD, os.name());
    OpResult::failed(pf::LOAD, code::OP_NOT_SUPPORTED, message)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde::Serialize;
    use serde_json::{Value, json};

    use super::*;
    use crate::request::MAX_REQUEST_BYTES;
    use crate::response::{OpResult, Outcome};
    use crate::system::FakeSystem;

    /// Runs the helper against a fresh `FakeSystem`, checks that the output
    /// is one JSON line, and returns it with the exit code. Nothing waits.
    fn helper(args: &[&OsStr], os: Os) -> (Value, u8, FakeSystem) {
        let fake = FakeSystem::default();
        let args: Vec<OsString> = args.iter().map(|arg| arg.to_os_string()).collect();
        let (line, code) = cli(&args, os, &fake, |_| panic!("the helper waited"));
        assert!(!line.contains('\n'));
        (serde_json::from_str(&line).unwrap(), code, fake)
    }

    /// A rejected request: exit 2, `request_invalid` with `reason`, and not a
    /// single call on the system.
    fn assert_rejected(raw: impl AsRef<OsStr>, reason: &str) {
        let (out, code, fake) = helper(&[OsStr::new("apply"), raw.as_ref()], Os::Macos);
        assert_eq!(code, 2);
        assert_eq!(out["error"]["code"], code::REQUEST_INVALID);
        let message = out["error"]["message"].as_str().unwrap();
        assert!(message.contains(reason), "{message}");
        assert!(fake.log.borrow().is_empty() && fake.files.borrow().is_empty());
    }

    fn with_ops(ops: Value) -> String {
        json!({ "version": 1, "ops": ops }).to_string()
    }

    fn one_op(op: Value) -> String {
        with_ops(json!([op]))
    }

    fn trust(pem: &str) -> String {
        one_op(json!({ "op": "trust_install", "pem": pem }))
    }

    fn hosts(names: impl Serialize) -> String {
        one_op(json!({ "op": "hosts_sync", "names": names }))
    }

    /// A valid request padded with spaces to exactly `len` bytes.
    fn padded(len: usize) -> String {
        let json = one_op(json!({ "op": "pf_remove" }));
        format!("{json}{}", " ".repeat(len - json.len()))
    }

    macro_rules! rejected {
        ($($name:ident: $raw:expr => $reason:literal,)*) => {$(
            #[test]
            fn $name() {
                assert_rejected($raw, $reason);
            }
        )*};
    }

    rejected! {
        broken_json: r#"{"version":1,"ops":["# => "is not valid JSON",
        empty_request: "" => "is not valid JSON",
        too_large: padded(MAX_REQUEST_BYTES + 1) => "larger than 65536 bytes",
        version_2: r#"{"version":2,"ops":[{"op":"pf_remove"}]}"# => "version: 2 is not supported",
        version_2_with_a_new_op: r#"{"version":2,"ops":[{"op":"future_op"}]}"#
            => "version: 2 is not supported",
        missing_version: r#"{"ops":[{"op":"pf_remove"}]}"# => "version: is missing",
        version_as_string: r#"{"version":"1","ops":[{"op":"pf_remove"}]}"# => "version: is missing",
        request_as_array: r#"[1,[{"op":"pf_remove"}]]"# => "request: is not an object",
        ops_not_an_array: r#"{"version":1,"ops":{"op":"pf_remove"}}"# => "ops: is missing or not",
        op_as_array: r#"{"version":1,"ops":[["pf_remove"]]}"# => "ops[0]: is not an object",
        op_as_array_after_an_object: r#"{"version":1,"ops":[{"op":"pf_remove"},["pf_remove"]]}"#
            => "ops[1]: is not an object",
        extra_request_field: r#"{"version":1,"ops":[{"op":"pf_remove"}],"x":1}"#
            => "does not match",
        unknown_op: one_op(json!({ "op": "run", "cmd": "rm -rf /" })) => "does not match",
        extra_field: one_op(json!({ "op": "resolver_install", "port": 18053, "cmd": "rm -rf /" }))
            => "does not match",
        extra_field_on_op_without_fields: one_op(json!({ "op": "pf_remove", "x": 1 }))
            => "does not match",
        duplicate_field: r#"{"version":1,"ops":[{"op":"resolver_install","port":18053,"port":80}]}"#
            => "does not match",
        duplicate_op_tag: r#"{"version":1,"ops":[{"op":"pf_remove","op":"trust_remove"}]}"#
            => "does not match",
        missing_field: one_op(json!({ "op": "resolver_install" })) => "does not match",
        port_as_string: one_op(json!({ "op": "resolver_install", "port": "18053" }))
            => "does not match",
        port_above_65535: one_op(json!({ "op": "resolved_install", "port": 65536 }))
            => "does not match",
        ports_install_with_a_port: one_op(json!({ "op": "ports_install", "port": 80 }))
            => "does not match",
        ports_install_with_a_path: one_op(json!({ "op": "ports_install", "path": "/etc/passwd" }))
            => "does not match",
        ports_remove_with_a_field: one_op(json!({ "op": "ports_remove", "all": true }))
            => "does not match",
        no_ops: with_ops(json!([])) => "ops: 0 ops, expected 1 to 16",
        seventeen_ops: with_ops(json!(vec![json!({ "op": "trust_remove" }); 17])) => "ops: 17 ops",
        port_80: one_op(json!({ "op": "resolver_install", "port": 80 }))
            => "ops[0].port: 80 is below 1024",
        port_0: one_op(json!({ "op": "pf_install", "http_port": 0, "https_port": 18443 }))
            => "ops[0].http_port: 0 is below 1024",
        same_ports: one_op(json!({ "op": "pf_install", "http_port": 18443, "https_port": 18443 }))
            => "ops[0]: http_port and https_port are the same",
        cert_path_instead_of_pem: one_op(json!({ "op": "trust_install", "cert": "/etc/ca.crt" }))
            => "does not match",
        empty_pem: trust("") => "ops[0].pem: is empty",
        pem_too_long: trust(&"A".repeat(16 * 1024 + 1)) => "ops[0].pem: is longer than 16384 bytes",
        pem_not_ascii: trust("-----BEGIN CERTIFICATE-----\né") => "ops[0].pem: is not ASCII",
        pem_with_nul: trust("-----BEGIN\u{0}") => "ops[0].pem: contains a NUL byte",
        name_with_capital: hosts(["Shop.test"]) => "ops[0].names[0]: is not <label>.test",
        name_not_test: hosts(["shop.com"]) => "is not <label>.test",
        name_with_leading_hyphen: hosts(["-shop.test"]) => "is not <label>.test",
        name_with_space: hosts(["a b.test"]) => "is not <label>.test",
        name_with_newline: hosts(["shop.test", "shop.test\n"])
            => "ops[0].names[1]: is not <label>.test",
        name_of_64_chars: hosts([format!("{}.test", "a".repeat(64))]) => "is not <label>.test",
        duplicate_name: hosts(["shop.test", "blog.test", "shop.test"])
            => "ops[0].names[2]: is a duplicate",
        too_many_names: hosts((0..1001).map(|i| format!("s{i}.test")).collect::<Vec<_>>())
            => "1001 names, at most 1000",
    }

    #[cfg(unix)]
    #[test]
    fn request_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        assert_rejected(OsStr::from_bytes(b"{\"version\":1\xff}"), "is not UTF-8");
    }

    #[cfg(windows)]
    #[test]
    fn request_not_utf8() {
        use std::os::windows::ffi::OsStringExt;
        assert_rejected(OsString::from_wide(&[0x7b, 0xd800, 0x7d]), "is not UTF-8");
    }

    #[test]
    fn a_request_of_exactly_the_limit_runs() {
        let raw = padded(MAX_REQUEST_BYTES);
        let (out, code, _) = helper(&[OsStr::new("apply"), OsStr::new(&raw)], Os::Macos);
        // The fake has no pfctl, so `pf_remove` runs and fails.
        assert_eq!(code, 1);
        assert_eq!(out["results"][0]["op"], "pf_remove");
    }

    #[test]
    fn a_valid_request_gives_one_result_per_op() {
        let raw = fixture("requests/macos-remove.json");
        let (out, code, _) = helper(&[OsStr::new("apply"), OsStr::new(&raw)], Os::Macos);
        assert_eq!(code, 1);
        let results = out["results"].as_array().unwrap();
        let ops: Vec<_> = results.iter().map(|r| &r["op"]).collect();
        assert_eq!(ops, ["pf_remove", "resolver_remove", "trust_remove"]);
    }

    #[test]
    fn version_prints_the_crate_version() {
        let fake = FakeSystem::default();
        let (line, code) = cli(&["version".into()], Os::Macos, &fake, |_| {});
        assert_eq!((line.as_str(), code), (env!("CARGO_PKG_VERSION"), 0));
    }

    #[test]
    fn pf_load_without_the_rules_file_does_nothing() {
        let (out, code, fake) = helper(&[OsStr::new("pf-load")], Os::Macos);
        assert_eq!(code, 1);
        assert_eq!(out["results"][0]["op"], "pf_load");
        assert_eq!(out["results"][0]["code"], pf::PF_NOT_INSTALLED);
        assert!(fake.log.borrow().is_empty());
    }

    #[test]
    fn pf_load_is_macos_only() {
        for os in [Os::Linux, Os::Windows] {
            let (out, code, fake) = helper(&[OsStr::new("pf-load")], os);
            assert_eq!(code, 1);
            assert_eq!(out["results"][0]["code"], code::OP_NOT_SUPPORTED);
            assert!(fake.log.borrow().is_empty());
        }
    }

    /// `main` passes its `sleep` to the boot job. The fake has no pfctl, so
    /// every load fails and is tried again after each wait.
    #[test]
    fn pf_load_waits_through_the_given_sleep() {
        let fake = FakeSystem::default();
        let rules = PathBuf::from("/Library/Application Support/Marsa/pf-redirect.rules");
        fake.files
            .borrow_mut()
            .insert(rules, (b"rdr".to_vec(), 0o644));
        let waits = RefCell::new(Vec::new());
        let sleep = |wait: Duration| waits.borrow_mut().push(wait.as_secs());
        let (line, code) = cli(&["pf-load".into()], Os::Macos, &fake, sleep);
        let out: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            (code, &out["results"][0]["code"]),
            (1, &json!(pf::PF_LOAD_FAILED))
        );
        assert_eq!(waits.into_inner(), [10, 30]);
        assert_eq!(fake.log.borrow().len(), 3);
    }

    #[test]
    fn a_bad_command_line_is_a_usage_error() {
        let ok = one_op(json!({ "op": "pf_remove" }));
        let cases: [&[&str]; 6] = [
            &[],
            &["run"],
            &["apply"],
            &["apply", &ok, &ok],
            &["version", "x"],
            &["pf-load", "x"],
        ];
        for args in cases {
            let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
            let (out, code, fake) = helper(&args, Os::Macos);
            assert_eq!(
                (code, &out["error"]["code"]),
                (2, &json!(code::USAGE)),
                "{args:?}"
            );
            assert!(fake.log.borrow().is_empty());
        }
    }

    /// Every file in `fixtures/helper/<folder>`, by name.
    fn fixtures(folder: &str) -> BTreeMap<String, String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let dir = root.join("fixtures").join("helper").join(folder);
        let entries = fs::read_dir(&dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()));
        // `.json` only, like marsa-core: Finder may leave a `.DS_Store` here.
        let files: BTreeMap<_, _> = entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, fs::read_to_string(&path).unwrap())
            })
            .collect();
        assert!(!files.is_empty(), "{}", dir.display());
        files
    }

    fn fixture(name: &str) -> String {
        let (folder, file) = name.split_once('/').unwrap();
        fixtures(folder).remove(file).unwrap()
    }

    /// The contract with `marsa-core` (task M4-06): every request fixture is
    /// valid on every OS.
    #[test]
    fn request_fixtures_are_valid() {
        for (file, text) in fixtures("requests") {
            let request = Request::from_arg(OsStr::new(&text));
            assert!(request.is_ok(), "{file}: {:?}", request.err());
        }
    }

    /// Every response fixture is exactly what the helper prints, with its exit
    /// code. A fixture without a case here fails the test.
    #[test]
    fn response_fixtures_match_the_output() {
        let apply = |ops: Value, os| {
            let raw = with_ops(ops);
            let (out, code, _) = helper(&[OsStr::new("apply"), OsStr::new(&raw)], os);
            (out, code)
        };
        let built = |results| {
            let response = Response::results(results);
            (
                serde_json::from_str(&response.to_json()).unwrap(),
                response.exit_code(),
            )
        };
        let done = |op| OpResult {
            op,
            outcome: Outcome::Done,
        };
        let unchanged = OpResult {
            op: "pf_install",
            outcome: Outcome::Unchanged,
        };
        let failed = ops::not_implemented("pf_install");
        let not_supported = json!([
            { "op": "pf_install", "http_port": 18080, "https_port": 18443 },
            { "op": "trust_remove" },
        ]);
        let port_80 = json!([{ "op": "pf_install", "http_port": 80, "https_port": 18443 }]);
        let (usage, usage_code, _) = helper(&[], Os::Macos);
        let cases = [
            (
                "done.json",
                built(vec![done("resolver_install"), unchanged]),
                0,
            ),
            (
                "failed.json",
                built(vec![done("resolver_install"), failed]),
                1,
            ),
            ("not-supported.json", apply(not_supported, Os::Linux), 1),
            ("rejected.json", apply(port_80, Os::Macos), 2),
            ("usage.json", (usage, usage_code), 2),
        ];
        let files = fixtures("responses");
        let names: Vec<_> = files.keys().map(String::as_str).collect();
        assert_eq!(names, cases.iter().map(|case| case.0).collect::<Vec<_>>());
        for (file, (out, code), expected_code) in cases {
            let fixture: Value = serde_json::from_str(&files[file]).unwrap();
            assert_eq!((out, code), (fixture, expected_code), "{file}");
        }
    }
}
