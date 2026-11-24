use std::fmt::Write as _;
use std::path::Path;

use super::*;

const STUB: &str = "Symfony\\Component\\VarDumper\\Cloner\\Stub";
const DATA: &str = "Symfony\\Component\\VarDumper\\Cloner\\Data";

/// Standard base64 with padding, to build lines like PHP's `base64_encode`.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `s:N:"..."`
fn s(text: &str) -> String {
    format!("s:{}:\"{text}\";", text.len())
}

/// A serialized `Stub` with these fields (`name:value` in serialize text).
fn stub(fields: &[(&str, &str)]) -> String {
    let mut out = format!("O:{}:\"{STUB}\":{}:{{", STUB.len(), fields.len());
    for (name, value) in fields {
        out += &format!("{}{value}", s(name));
    }
    out + "}"
}

/// A whole line: `rows` is the serialized `data` array, `context` the
/// serialized second element, and `data_context` the Data's own context.
fn line_with(rows: &str, context: &str, data_context: &str) -> Vec<u8> {
    let prop = |name: &str| s(&format!("\0{DATA}\0{name}"));
    let text = format!(
        "a:2:{{i:0;O:{}:\"{DATA}\":7:{{{}{rows}{}i:0;{}i:0;{}i:20;{}i:-1;{}i:-1;{}{data_context}}}i:1;{context}}}",
        DATA.len(),
        prop("data"),
        prop("position"),
        prop("key"),
        prop("maxDepth"),
        prop("maxItemsPerDepth"),
        prop("useRefHandles"),
        prop("context"),
    );
    base64(text.as_bytes()).into_bytes()
}

fn line(rows: &str) -> Vec<u8> {
    line_with(rows, "a:0:{}", "a:0:{}")
}

/// The text of a dump made of these rows.
fn text(rows: &str) -> String {
    parse_line(&line(rows)).expect("a dump").text
}

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dumps")
}

/// Every line of `name.<version>.dump`, written as the window shows it.
fn describe(name: &str, version: &str) -> String {
    let path = fixtures().join(format!("{name}.{version}.dump"));
    let lines = std::fs::read_to_string(&path).expect("read the fixture");
    let mut out = String::new();
    for (i, line) in lines.lines().enumerate() {
        let dump = parse_line(line.as_bytes()).expect("a fixture is a dump");
        let _ = writeln!(out, "--- dump {} ---", i + 1);
        let _ = writeln!(
            out,
            "source: {}:{}",
            dump.source_file.as_deref().unwrap_or("-"),
            dump.source_line.map_or("-".into(), |n| n.to_string())
        );
        let _ = writeln!(
            out,
            "request: {} {}",
            dump.method.as_deref().unwrap_or("-"),
            dump.uri.as_deref().unwrap_or("-")
        );
        let _ = writeln!(out, "label: {}", dump.label.as_deref().unwrap_or("-"));
        let _ = writeln!(out, "truncated: {}", dump.truncated);
        let _ = writeln!(out, "{}", dump.text);
    }
    out
}

#[test]
fn every_fixture_of_symfony_6_4_and_7_4_reads() {
    for version in ["v64", "v74"] {
        insta::with_settings!({ snapshot_suffix => version }, {
            insta::assert_snapshot!("scalars", describe("scalars", version));
            insta::assert_snapshot!("array", describe("array", version));
            insta::assert_snapshot!("object", describe("object", version));
            insta::assert_snapshot!("circular", describe("circular", version));
            insta::assert_snapshot!("utf8", describe("utf8", version));
            insta::assert_snapshot!("binary", describe("binary", version));
            insta::assert_snapshot!("label", describe("label", version));
            insta::assert_snapshot!("multi", describe("multi", version));
            insta::assert_snapshot!("web_request", describe("object_web", version));
        });
    }
}

#[test]
fn a_long_text_is_kept_whole_under_the_limit() {
    let dump = parse_line(
        std::fs::read_to_string(fixtures().join("long_string.v74.dump"))
            .expect("read")
            .trim()
            .as_bytes(),
    )
    .expect("a dump");
    assert_eq!(dump.text, format!("\"{}\"", "x".repeat(5000)));
    assert!(!dump.truncated);
}

#[test]
fn both_versions_write_the_same_text_for_the_same_value() {
    for name in ["scalars", "array", "utf8", "binary", "multi"] {
        assert_eq!(describe(name, "v64"), describe(name, "v74"), "{name}");
    }
}

#[test]
fn context_comes_from_the_line_and_is_cleaned() {
    let context = format!(
        "a:3:{{{}d:1795492984.9;{}a:2:{{{}{}{}i:7;}}{}a:2:{{{}{}{}{}}}}}",
        s("timestamp"),
        s("source"),
        s("file"),
        s("/app/a\u{1b}[2J.php"),
        s("line"),
        s("request"),
        s("uri"),
        s("http://shop.test/a?b=1"),
        s("method"),
        s("GET"),
    );
    let rows = "a:1:{i:0;a:1:{i:0;N;}}";
    let dump = parse_line(&line_with(rows, &context, "a:0:{}")).expect("a dump");
    assert_eq!(dump.received_at, 1_795_492_984_900);
    assert_eq!(dump.source_file.as_deref(), Some("/app/a[2J.php"), "no ESC");
    assert_eq!(dump.source_line, Some(7));
    assert_eq!(dump.uri.as_deref(), Some("http://shop.test/a?b=1"));
    assert_eq!(dump.method.as_deref(), Some("GET"));
    assert_eq!(dump.id, 0);
}

#[test]
fn a_label_comes_from_the_data_context_and_is_cut() {
    let rows = "a:1:{i:0;a:1:{i:0;N;}}";
    let label = "l".repeat(1000);
    let dump = parse_line(&line_with(
        rows,
        "a:0:{}",
        &format!("a:1:{{{}{}}}", s("label"), s(&label)),
    ))
    .expect("a dump");
    assert_eq!(dump.label.as_deref().map(str::len), Some(256));
}

#[test]
fn the_marks_of_properties_follow_cli_dumper() {
    let props = format!(
        "a:5:{{{}i:1;{}i:2;{}i:3;{}i:4;{}i:5;}}",
        s("pub"),
        s("\0*\0prot"),
        s("\0Some\\Class\0priv"),
        s("\0+\0dyn"),
        s("\0~\0note"),
    );
    let rows = format!(
        "a:2:{{i:0;a:1:{{i:0;{}}}i:1;{props}}}",
        stub(&[
            ("type", "i:4;"),
            ("class", &s("Foo\\Bar")),
            ("handle", "i:9;"),
            ("position", "i:1;"),
        ])
    );
    assert_eq!(
        text(&rows),
        "Foo\\Bar {#9\n  +pub: 1\n  #prot: 2\n  -priv: 3\n  +\"dyn\": 4\n  note: 5\n}"
    );
}

#[test]
fn an_empty_object_a_std_class_and_an_empty_array() {
    let object = |class: &str| {
        text(&format!(
            "a:1:{{i:0;a:1:{{i:0;{}}}}}",
            stub(&[("type", "i:4;"), ("class", &s(class)), ("handle", "i:3;")])
        ))
    };
    assert_eq!(object("Foo"), "Foo {#3}");
    assert_eq!(object("stdClass"), "{#3}");
    assert_eq!(text("a:1:{i:0;a:1:{i:0;a:0:{}}}"), "[]");
}

#[test]
fn items_the_cloner_left_out_are_counted() {
    // `[1 => row 1, 0 => 5]`: an array of two shown items and five cut.
    let rows = "a:2:{i:0;a:1:{i:0;a:2:{i:1;i:1;i:0;i:5;}}i:1;a:2:{i:0;i:10;i:1;i:20;}}";
    assert_eq!(text(rows), "array:7 [\n  0 => 10\n  1 => 20\n  …5\n]");
    // An object cut at its depth: no children, only the mark.
    let cut = stub(&[("type", "i:4;"), ("class", &s("Deep")), ("cut", "i:3;")]);
    assert_eq!(
        text(&format!("a:1:{{i:0;a:1:{{i:0;{cut}}}}}")),
        "Deep { …3}"
    );
}

#[test]
fn a_string_with_line_breaks_is_a_block_and_one_with_a_final_break_is_not() {
    assert_eq!(
        text(&format!("a:1:{{i:0;a:1:{{i:0;{}}}}}", s("one\ntwo"))),
        "\"\"\"\none\ntwo\n\"\"\""
    );
    assert_eq!(
        text(&format!("a:1:{{i:0;a:1:{{i:0;{}}}}}", s("one\n"))),
        "\"one\\n\""
    );
    // Control characters never reach the text as they are.
    assert_eq!(
        text(&format!(
            "a:1:{{i:0;a:1:{{i:0;{}}}}}",
            s("a\u{1b}[2Jb\u{7}")
        )),
        "\"a\\e[2Jb\\x07\""
    );
}

#[test]
fn a_cut_string_and_a_binary_one() {
    let long = stub(&[
        ("type", "i:2;"),
        ("class", "i:2;"),
        ("value", &s("abc")),
        ("cut", "i:40;"),
    ]);
    assert_eq!(
        text(&format!("a:1:{{i:0;a:1:{{i:0;{long}}}}}")),
        "\"abc\"…40"
    );
    let binary = stub(&[
        ("type", "i:2;"),
        ("class", "i:1;"),
        ("value", &s("a\0\u{ff}")),
    ]);
    assert_eq!(
        text(&format!("a:1:{{i:0;a:1:{{i:0;{binary}}}}}")),
        "b\"a\\x00\u{c3}\u{bf}\""
    );
}

#[test]
fn a_resource_and_a_reference() {
    let resource = stub(&[
        ("type", "i:5;"),
        ("class", &s("stream")),
        ("handle", "i:12;"),
        ("position", "i:1;"),
    ]);
    let rows = format!(
        "a:2:{{i:0;a:1:{{i:0;{resource}}}i:1;a:1:{{{}{}}}}}",
        s("mode"),
        s("r")
    );
    assert_eq!(text(&rows), "stream resource @12 {\n  mode: \"r\"\n}");
    let reference = stub(&[("type", "i:1;"), ("handle", "i:99;"), ("value", "i:5;")]);
    assert_eq!(
        text(&format!("a:1:{{i:0;a:1:{{i:0;{reference}}}}}")),
        "&1 5"
    );
}

#[test]
fn floats_are_written_as_php_writes_them() {
    for (serialized, shown) in [
        ("d:1;", "1.0"),
        ("d:3.14;", "3.14"),
        ("d:1.0E+25;", "1.0E+25"),
        ("d:-INF;", "-INF"),
        ("d:NAN;", "NAN"),
    ] {
        assert_eq!(
            text(&format!("a:1:{{i:0;a:1:{{i:0;{serialized}}}}}")),
            shown
        );
    }
}

#[test]
fn a_row_that_contains_itself_ends_at_the_depth_limit() {
    let looping = stub(&[
        ("type", "i:4;"),
        ("class", &s("Loop")),
        ("position", "i:1;"),
    ]);
    let rows = format!(
        "a:2:{{i:0;a:1:{{i:0;{looping}}}i:1;a:1:{{{}{looping}}}}}",
        s("next")
    );
    let dump = parse_line(&line(&rows)).expect("a dump");
    assert!(dump.text.contains('…'), "the nesting is cut");
    assert!(dump.text.len() < 20_000);
}

#[test]
fn a_text_over_the_limit_is_cut_and_marked() {
    // One string of 300 KiB.
    let big = "y".repeat(300 * 1024);
    let dump = parse_line(&line(&format!("a:1:{{i:0;a:1:{{i:0;{}}}}}", s(&big)))).expect("a dump");
    assert!(dump.truncated);
    assert!(dump.text.len() <= MAX_TEXT_BYTES);
    // 30 000 short items: cut by lines.
    let mut row = String::from("a:30000:{");
    for i in 0..30_000 {
        let _ = write!(row, "i:{i};i:{i};");
    }
    row.push('}');
    let dump = parse_line(&line(&format!(
        "a:2:{{i:0;a:1:{{i:0;a:1:{{i:2;i:1;}}}}i:1;{row}}}"
    )))
    .expect("a dump");
    assert!(dump.truncated);
    assert!(dump.text.lines().count() <= MAX_TEXT_LINES);
}

#[test]
fn text_that_is_not_utf8_is_shown_with_replacement_characters() {
    // A string node holding the byte 0xFF, which is not UTF-8. `s()` takes
    // UTF-8 text, so the line is built from bytes.
    let mut bytes = format!("a:2:{{i:0;O:{}:\"{DATA}\":2:{{", DATA.len()).into_bytes();
    bytes.extend_from_slice(s(&format!("\0{DATA}\0data")).as_bytes());
    bytes.extend_from_slice(b"a:1:{i:0;a:1:{i:0;s:3:\"a\xffb\";}}");
    bytes.extend_from_slice(s(&format!("\0{DATA}\0position")).as_bytes());
    bytes.extend_from_slice(b"i:0;}i:1;a:0:{}}");
    let dump = parse_line(base64(&bytes).as_bytes()).expect("a dump");
    assert_eq!(dump.text, "\"a\u{fffd}b\"");
}

#[test]
fn what_is_not_a_dump_is_refused_with_its_code() {
    let ok_rows = "a:1:{i:0;a:1:{i:0;N;}}";
    let invalid = [
        ("an empty line", Vec::new()),
        ("not base64", b"!!!! not base64".to_vec()),
        ("base64 of text", base64(b"hello").into_bytes()),
        ("serialize that is not a pair", base64(b"i:1;").into_bytes()),
        (
            "a custom serialization",
            base64(b"a:2:{i:0;C:3:\"Foo\":0:{}i:1;a:0:{}}").into_bytes(),
        ),
        (
            "data that is not Data",
            base64(b"a:2:{i:0;N;i:1;a:0:{}}").into_bytes(),
        ),
        (
            "an object that is not Data",
            base64(b"a:2:{i:0;O:3:\"Foo\":0:{}i:1;a:0:{}}").into_bytes(),
        ),
        (
            "a huge declared length",
            base64(b"a:2:{i:0;s:999999999:\"x\";i:1;a:0:{}}").into_bytes(),
        ),
        (
            "an item that is not a stub",
            line("a:1:{i:0;a:1:{i:0;O:3:\"Foo\":0:{}}}"),
        ),
        ("a root that is missing", line("a:0:{}")),
        ("rows that are not a list of arrays", line("a:1:{i:0;i:5;}")),
        (
            "a stub of a type that does not exist",
            line(&format!(
                "a:1:{{i:0;a:1:{{i:0;{}}}}}",
                stub(&[("type", "i:77;")])
            )),
        ),
    ];
    for (why, bytes) in invalid {
        let err = parse_line(&bytes).expect_err(why);
        assert_eq!(err.code(), crate::codes::DUMP_INVALID, "{why}");
    }
    assert!(parse_line(&line(ok_rows)).is_ok());
}

#[test]
fn nesting_over_512_is_refused() {
    let mut nested = String::new();
    for _ in 0..600 {
        nested.push_str("a:1:{i:0;");
    }
    nested.push_str("N;");
    nested.push_str(&"}".repeat(600));
    let err = parse_line(base64(nested.as_bytes()).as_bytes()).expect_err("too deep");
    assert_eq!(err, DumpError::Invalid("nested too deep"));
}

#[test]
fn a_line_over_1_mib_is_too_large_and_one_at_the_limit_is_read_as_a_line() {
    let big = vec![b'A'; MAX_LINE_BYTES + 1];
    let err = parse_line(&big).expect_err("too large");
    assert_eq!(err, DumpError::TooLarge);
    assert_eq!(err.code(), crate::codes::DUMP_TOO_LARGE);
    // At the limit it is not "too large" (it is just not a dump).
    let at = vec![b'A'; MAX_LINE_BYTES];
    assert_eq!(
        parse_line(&at).expect_err("not a dump").code(),
        crate::codes::DUMP_INVALID
    );
}

#[test]
fn a_line_break_after_the_line_is_fine() {
    let mut with = line("a:1:{i:0;a:1:{i:0;b:1;}}");
    with.extend_from_slice(b"\r\n");
    assert_eq!(parse_line(&with).expect("a dump").text, "true");
}

#[test]
fn random_lines_never_panic() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    // Mutations of a real line: flip, drop, or repeat bytes of its decoded text.
    let real = std::fs::read_to_string(fixtures().join("object.v74.dump")).expect("read");
    let original = crate::base64::decode(real.trim().as_bytes()).expect("base64");
    for _ in 0..3_000 {
        let mut bytes = original.clone();
        for _ in 0..(next() % 4 + 1) {
            let at = (next() as usize) % bytes.len();
            match next() % 3 {
                0 => bytes[at] = next() as u8,
                1 => {
                    bytes.remove(at);
                }
                _ => bytes.insert(at, b"0123456789;:{}\"\0"[(next() % 15) as usize]),
            }
        }
        let _ = parse_line(base64(&bytes).as_bytes());
    }
    for _ in 0..1_000 {
        let len = (next() % 200) as usize;
        let noise: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let _ = parse_line(&noise);
        let _ = parse_line(base64(&noise).as_bytes());
    }
}
