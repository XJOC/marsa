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
//! ```
//!
//! The request is one argument, not stdin: osascript's `do shell script` has
//! no stdin, and `quoted form of` keeps the argument whole. Exit codes: `0`
//! every op succeeded, `1` an op failed, `2` a rejected request or a bad
//! command line.

mod ops;
mod os;
mod request;
mod response;
mod system;

use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

use crate::os::Os;
use crate::request::Request;
use crate::response::Response;
use crate::system::{RealSystem, System};

const USAGE: &str = "usage: marsa-helper apply <json> | pf-load | version";

fn main() -> ExitCode {
    // `args_os`: `args` panics on an argument that is not UTF-8.
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let (line, code) = cli(&args, Os::current(), &RealSystem);
    // stdout is the helper's protocol: this one line is all it prints there.
    println!("{line}");
    ExitCode::from(code)
}

/// Runs one command line. Returns the line for stdout and the exit code.
fn cli(args: &[OsString], os: Os, system: &impl System) -> (String, u8) {
    let response = match args {
        [cmd] if cmd == "version" => return (env!("CARGO_PKG_VERSION").to_owned(), 0),
        [cmd] if cmd == "pf-load" => Response::results(vec![ops::not_implemented("pf_load")]),
        [cmd, raw] if cmd == "apply" => match Request::from_arg(raw) {
            Ok(request) => Response::results(ops::apply(&request, os, system)),
            Err(err) => Response::error(err.code(), err.to_string()),
        },
        _ => Response::error("usage", USAGE.to_owned()),
    };
    (response.to_json(), response.exit_code())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    use serde::Serialize;
    use serde_json::{Value, json};

    use super::*;
    use crate::request::MAX_REQUEST_BYTES;
    use crate::response::{OpResult, Outcome};
    use crate::system::FakeSystem;

    #[cfg(not(windows))]
    const HOME: &str = "/Users/example";
    #[cfg(windows)]
    const HOME: &str = r"C:\Users\example";

    /// Runs the helper against a fresh `FakeSystem`, checks that the output
    /// is one JSON line, and returns it with the exit code.
    fn helper(args: &[&OsStr], os: Os) -> (Value, u8, FakeSystem) {
        let fake = FakeSystem::default();
        let args: Vec<OsString> = args.iter().map(|arg| arg.to_os_string()).collect();
        let (line, code) = cli(&args, os, &fake);
        assert!(!line.contains('\n'));
        (serde_json::from_str(&line).unwrap(), code, fake)
    }

    /// A rejected request: exit 2, `request_invalid` with `reason`, and not a
    /// single call on the system.
    fn assert_rejected(raw: impl AsRef<OsStr>, reason: &str) {
        let (out, code, fake) = helper(&[OsStr::new("apply"), raw.as_ref()], Os::Macos);
        assert_eq!(code, 2);
        assert_eq!(out["error"]["code"], "request_invalid");
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

    fn trust(cert: impl AsRef<Path>) -> String {
        one_op(json!({ "op": "trust_install", "cert": cert.as_ref() }))
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
        missing_version: r#"{"ops":[{"op":"pf_remove"}]}"# => "does not match",
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
        no_ops: with_ops(json!([])) => "ops: 0 ops, expected 1 to 16",
        seventeen_ops: with_ops(json!(vec![json!({ "op": "trust_remove" }); 17])) => "ops: 17 ops",
        port_80: one_op(json!({ "op": "resolver_install", "port": 80 }))
            => "ops[0].port: 80 is below 1024",
        port_0: one_op(json!({ "op": "pf_install", "http_port": 0, "https_port": 18443 }))
            => "ops[0].http_port: 0 is below 1024",
        same_ports: one_op(json!({ "op": "pf_install", "http_port": 18443, "https_port": 18443 }))
            => "ops[0]: http_port and https_port are the same",
        relative_cert: trust("caddy/root.crt") => "ops[0].cert: is not an absolute path",
        cert_with_parent_dir: trust(Path::new(HOME).join("..").join("root.crt")) => "`..`",
        cert_with_nul: trust(format!("{HOME}/root\u{0}.crt")) => "NUL",
        cert_too_long: trust(Path::new(HOME).join("a".repeat(1024))) => "longer than 1024 bytes",
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

    /// As SYSTEM, reading `\\server\share` would connect to another machine.
    #[cfg(windows)]
    #[test]
    fn cert_not_on_a_local_drive() {
        assert_rejected(trust(r"\\server\share\root.crt"), "is not on a local drive");
        assert_rejected(trust(r"\\?\C:\root.crt"), "is not on a local drive");
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
        assert_eq!(code, 1);
        assert_eq!(out["results"][0]["code"], "op_not_implemented");
    }

    #[test]
    fn a_valid_request_gives_one_result_per_op() {
        // No paths in this fixture, so it is valid on every OS.
        let raw = fixture("requests/macos-remove.json");
        let (out, code, fake) = helper(&[OsStr::new("apply"), OsStr::new(&raw)], Os::Macos);
        assert_eq!(code, 1);
        let results = out["results"].as_array().unwrap();
        let ops: Vec<_> = results.iter().map(|r| &r["op"]).collect();
        assert_eq!(ops, ["pf_remove", "resolver_remove", "trust_remove"]);
        assert!(fake.log.borrow().is_empty());
    }

    #[test]
    fn version_prints_the_crate_version() {
        let fake = FakeSystem::default();
        let (line, code) = cli(&["version".into()], Os::Macos, &fake);
        assert_eq!((line.as_str(), code), (env!("CARGO_PKG_VERSION"), 0));
    }

    #[test]
    fn pf_load_is_not_implemented_yet() {
        let (out, code, fake) = helper(&[OsStr::new("pf-load")], Os::Macos);
        assert_eq!(code, 1);
        assert_eq!(out["results"][0]["op"], "pf_load");
        assert_eq!(out["results"][0]["code"], "op_not_implemented");
        assert!(fake.log.borrow().is_empty());
    }

    #[test]
    fn a_bad_command_line_is_a_usage_error() {
        let ok = one_op(json!({ "op": "pf_remove" }));
        let cases: [&[&str]; 5] = [
            &[],
            &["run"],
            &["apply"],
            &["apply", &ok, &ok],
            &["version", "x"],
        ];
        for args in cases {
            let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
            let (out, code, fake) = helper(&args, Os::Macos);
            assert_eq!(
                (code, &out["error"]["code"]),
                (2, &json!("usage")),
                "{args:?}"
            );
            assert!(fake.log.borrow().is_empty());
        }
    }

    fn fixture(name: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let path = root.join("fixtures").join("helper").join(name);
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    }

    /// The contract with `marsa-core` (task M4-06): every request fixture
    /// parses, and those with paths for this OS family pass validation.
    #[test]
    fn request_fixtures_parse_and_validate() {
        let files = [
            ("macos-setup.json", cfg!(unix)),
            ("macos-remove.json", true),
            ("linux-setup.json", cfg!(unix)),
            ("windows-hosts.json", true),
            ("uninstall.json", true),
        ];
        for (file, valid_here) in files {
            let text = fixture(&format!("requests/{file}"));
            let request = Request::parse(OsStr::new(&text)).unwrap();
            assert_eq!(request.validate().is_ok(), valid_here, "{file}");
        }
    }

    #[test]
    fn response_fixtures_match_the_output() {
        let done = |op| OpResult {
            op,
            outcome: Outcome::Done,
        };
        let unchanged = OpResult {
            op: "pf_install",
            outcome: Outcome::Unchanged,
        };
        let message = "pf_install is not implemented yet".to_owned();
        let failed = OpResult::failed("pf_install", "op_not_implemented", message);
        let reason = "ops[0].http_port: 80 is below 1024".to_owned();
        let cases = [
            (
                "done.json",
                Response::results(vec![done("resolver_install"), unchanged]),
                0,
            ),
            (
                "failed.json",
                Response::results(vec![done("resolver_install"), failed]),
                1,
            ),
            (
                "rejected.json",
                Response::error("request_invalid", reason),
                2,
            ),
        ];
        for (file, response, code) in cases {
            let expected: Value =
                serde_json::from_str(&fixture(&format!("responses/{file}"))).unwrap();
            let actual: Value = serde_json::from_str(&response.to_json()).unwrap();
            assert_eq!((actual, response.exit_code()), (expected, code), "{file}");
        }
    }
}
