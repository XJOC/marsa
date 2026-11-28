//! The Valkey routes against a Valkey played by the test: a listener on
//! `127.0.0.1` that reads each command as Valkey does and answers what the
//! test says. The real server is read in `tools/e2e/valkey.sh`.

use std::io;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use marsa_core::databases::ServerKind;
use marsa_core::databases::valkey::command;
use marsa_core::db_browser::resp::{Decoder, Reply};
use marsa_core::db_browser::{
    ELEMENT_BYTES, KEY_BYTES, KEY_ELEMENTS, KeyCommand, SCAN_CALLS, VALUE_BYTES,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

use crate::daemon::db_browser::tests::App;

/// One command as it arrived: its words, and the connection that sent it.
type Seen = Arc<Mutex<Vec<(usize, Vec<String>)>>>;
/// What the fake answers a command with: the bytes; [`hang_up`]'s bytes,
/// after which it closes; or nothing, which closes the connection (but
/// `SELECT` is then answered `+OK`).
type Script = dyn Fn(&[&str]) -> Option<Vec<u8>> + Send + Sync;

struct Valkey {
    app: App,
    seen: Seen,
    /// Connections that were opened, and that ended.
    opened: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
}

/// `bytes`, then the end of the connection.
fn hang_up(bytes: &[u8]) -> Vec<u8> {
    [&[0], bytes].concat()
}

/// A daemon whose Valkey runs on a listener that answers with `script`.
async fn valkey(script: impl Fn(&[&str]) -> Option<Vec<u8>> + Send + Sync + 'static) -> Valkey {
    let app = App::new();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    app.daemon
        .server(ServerKind::Valkey)
        .pretend_ready(&app.daemon, port);
    let seen = Seen::default();
    let opened = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let script: Arc<Script> = Arc::new(script);
    tokio::spawn({
        let (seen, opened, closed) = (seen.clone(), opened.clone(), closed.clone());
        async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let number = opened.fetch_add(1, Ordering::SeqCst) + 1;
                let (seen, closed, script) = (seen.clone(), closed.clone(), script.clone());
                tokio::spawn(async move {
                    let mut chunk = vec![0; 64 * 1024];
                    // One command at a time: the daemon waits for each answer.
                    let mut decoder = request_decoder();
                    loop {
                        let read = stream.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        let Some(request) = decoder.feed(&chunk[..read]).unwrap() else {
                            continue;
                        };
                        decoder = request_decoder();
                        let words = words(request);
                        seen.lock().unwrap().push((number, words.clone()));
                        let words: Vec<&str> = words.iter().map(String::as_str).collect();
                        let reply = script(&words)
                            .or_else(|| (words[0] == "SELECT").then(|| b"+OK\r\n".to_vec()));
                        let Some(reply) = reply else { break };
                        let (last, reply) = match reply.strip_prefix(&[0]) {
                            Some(rest) => (true, rest),
                            None => (false, reply.as_slice()),
                        };
                        if stream.write_all(reply).await.is_err() || last {
                            break;
                        }
                    }
                    closed.fetch_add(1, Ordering::SeqCst);
                });
            }
        }
    });
    Valkey {
        app,
        seen,
        opened,
        closed,
    }
}

fn request_decoder() -> Decoder {
    let mut limits = KeyCommand::DbSize.limits();
    limits.keep = 2 * KEY_BYTES;
    Decoder::new(limits)
}

fn words(request: Reply) -> Vec<String> {
    let Reply::Array(words) = request else {
        panic!("a command is an array: {request:?}");
    };
    words
        .into_iter()
        .map(|word| match word {
            Reply::Bulk(bulk) => String::from_utf8(bulk.bytes).unwrap(),
            other => panic!("a word is a string: {other:?}"),
        })
        .collect()
}

impl Valkey {
    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.app.router.answer("POST", path, Some(body)).await
    }

    async fn keys(&self, body: Value) -> (StatusCode, Value) {
        self.post("/v1/db/keys", body).await
    }

    async fn key(&self, name: &str) -> (StatusCode, Value) {
        self.post("/v1/db/key", json!({ "db": 0, "key": name }))
            .await
    }

    /// The commands that arrived, each as one line.
    fn lines(&self) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|(_, words)| words.join(" ")).collect()
    }

    async fn until_closed(&self, connections: usize) {
        for _ in 0..500 {
            if self.closed.load(Ordering::SeqCst) >= connections {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the connection stayed open");
    }
}

fn integer(number: i64) -> Vec<u8> {
    format!(":{number}\r\n").into_bytes()
}

fn status(word: &str) -> Vec<u8> {
    format!("+{word}\r\n").into_bytes()
}

fn strings(values: &[&str]) -> Vec<u8> {
    command(values)
}

fn string(value: &str) -> Vec<u8> {
    format!("${}\r\n{value}\r\n", value.len()).into_bytes()
}

/// The answer of `SCAN`, `HSCAN`, and `SSCAN`.
fn scanned(cursor: &str, found: &[&str]) -> Vec<u8> {
    [b"*2\r\n".to_vec(), string(cursor), strings(found)].concat()
}

/// A Valkey with one key of each type.
fn shop(words: &[&str]) -> Option<Vec<u8>> {
    let reply = match words {
        ["DBSIZE"] => integer(6),
        ["SCAN", "0", ..] => scanned("0", &["name", "cart", "jobs", "ids", "top", "log"]),
        ["TYPE", key] => status(match *key {
            "name" => "string",
            "cart" => "hash",
            "jobs" => "list",
            "ids" => "set",
            "top" => "zset",
            "log" => "stream",
            "bloom" => "MBbloom--",
            _ => "none",
        }),
        ["TTL", "cart"] => integer(120),
        ["TTL", _] => integer(-1),
        ["STRLEN", _] => integer(5),
        ["GETRANGE", ..] => string("Marsa"),
        ["HLEN", _] => integer(2),
        ["HSCAN", _, "0", ..] => scanned("7", &["sku", "14"]),
        ["HSCAN", _, "7", ..] => scanned("0", &["qty", "2"]),
        ["LLEN", _] => integer(900),
        ["LRANGE", ..] => strings(&["first", "second"]),
        ["SCARD", _] => integer(1),
        ["SSCAN", ..] => scanned("0", &["12"]),
        ["ZCARD", _] => integer(1),
        ["ZRANGE", ..] => strings(&["omar", "12.5"]),
        _ => return None,
    };
    Some(reply)
}

#[tokio::test]
async fn a_stopped_valkey_is_listed_without_a_connection() {
    let app = App::new();
    let version = app.dir.path().join("data/runtimes/valkey/8.1.11");
    std::fs::create_dir_all(version).unwrap();
    let (status, json) = app.router.answer("GET", "/v1/db/sources", None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["valkey"]["state"], "stopped", "{json}");
    assert_eq!(json["valkey"]["databases"], json!([]));
    assert!(json["valkey"].get("problem").is_none(), "{json}");
    for (path, body) in [
        ("/v1/db/keys", json!({ "db": 0 })),
        ("/v1/db/key", json!({ "db": 0, "key": "a" })),
    ] {
        let (status, json) = app.router.answer("POST", path, Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{json}");
        assert_eq!(json["error"]["code"], "db_server_not_running");
    }
    assert!(app.daemon.open_db_browser().is_none());
}

/// No route takes a command, an address, a database Valkey does not have, or
/// a search or a name past its limit. Nothing reaches the server.
#[tokio::test]
async fn a_bad_key_request_never_reaches_valkey() {
    let vk = valkey(shop).await;
    let long = "k".repeat(KEY_BYTES + 1);
    for (path, body, status, code) in [
        (
            "/v1/db/keys",
            json!({ "db": 0, "command": "FLUSHALL" }),
            422,
            "invalid_body",
        ),
        (
            "/v1/db/key",
            json!({ "db": 0, "key": "a", "args": ["FLUSHALL"] }),
            422,
            "invalid_body",
        ),
        ("/v1/db/keys", json!({ "db": 16 }), 400, "db_source_invalid"),
        (
            "/v1/db/key",
            json!({ "db": 255, "key": "a" }),
            400,
            "db_source_invalid",
        ),
        (
            "/v1/db/keys",
            json!({ "db": 0, "pattern": "*".repeat(1025) }),
            400,
            "db_pattern_too_long",
        ),
        (
            "/v1/db/key",
            json!({ "db": 0, "key": long }),
            400,
            "db_source_invalid",
        ),
    ] {
        let (got, json) = vk.post(path, body).await;
        assert_eq!(got.as_u16(), status, "{path}: {json}");
        assert_eq!(json["error"]["code"], code, "{path}");
    }
    assert!(vk.lines().is_empty(), "{:?}", vk.lines());
}

#[tokio::test]
async fn a_page_of_keys_comes_with_the_database_total() {
    let vk = valkey(shop).await;
    let (status, json) = vk.keys(json!({ "db": 1 })).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(
        json,
        json!({
            "keys": ["name", "cart", "jobs", "ids", "top", "log"],
            "cursor": 0, "unshown": 0, "total": 6,
        })
    );
    assert_eq!(vk.lines(), ["SELECT 1", "SCAN 0 COUNT 100", "DBSIZE"]);
}

/// A pattern with spaces, line breaks, and a command in it is the one word
/// after `MATCH`.
#[tokio::test]
async fn a_pattern_reaches_valkey_as_one_word() {
    let vk = valkey(shop).await;
    let pattern = "a b\r\n*1\r\n$8\r\nFLUSHALL\r\n";
    let body = json!({ "db": 0, "pattern": pattern, "kind": "list" });
    let (status, json) = vk.keys(body).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let seen = vk.seen.lock().unwrap();
    assert_eq!(
        seen[1].1,
        [
            "SCAN", "0", "MATCH", pattern, "COUNT", "100", "TYPE", "list"
        ]
    );
    assert_eq!(seen.len(), 3);
}

/// `SCAN` is asked until the page is full; then the cursor goes back to the
/// client, and a request goes on from it.
#[tokio::test]
async fn a_page_asks_scan_until_it_is_full() {
    let names: Vec<String> = (0..150).map(|n| format!("k{n}")).collect();
    let vk = valkey(move |words| match words {
        ["DBSIZE"] => Some(integer(100_000)),
        ["SCAN", cursor, ..] => {
            let next = cursor.parse::<u64>().unwrap() + 1;
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            Some(scanned(&next.to_string(), &names))
        }
        _ => None,
    })
    .await;
    let (_, json) = vk.keys(json!({ "db": 0 })).await;
    // 150 a call: the third call passes 400, and nothing it gave is dropped.
    assert_eq!(json["keys"].as_array().unwrap().len(), 450);
    assert_eq!(json["cursor"], 3);
    assert_eq!(json["total"], 100_000);
    let (_, json) = vk.keys(json!({ "db": 0, "cursor": 3 })).await;
    assert_eq!(json["cursor"], 6);
    assert!(vk.lines().contains(&"SCAN 3 COUNT 100".to_owned()));
}

/// A search that finds nothing stops after its calls and says where to go on.
#[tokio::test]
async fn a_search_that_finds_little_stops_with_a_cursor() {
    let vk = valkey(|words| match words {
        ["DBSIZE"] => Some(integer(9)),
        ["SCAN", ..] => Some(scanned("5", &[])),
        _ => None,
    })
    .await;
    let (status, json) = vk.keys(json!({ "db": 0, "pattern": "x" })).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["keys"], json!([]));
    assert_eq!(json["cursor"], 5);
    let scans = vk.lines().iter().filter(|l| l.starts_with("SCAN")).count();
    assert_eq!(scans, SCAN_CALLS as usize);
}

#[tokio::test]
async fn a_name_that_cannot_be_shown_is_counted() {
    let long = "k".repeat(KEY_BYTES + 100);
    let vk = valkey(move |words| match words {
        ["DBSIZE"] => Some(integer(3)),
        ["SCAN", ..] => Some(
            [
                b"*2\r\n$1\r\n0\r\n*3\r\n$2\r\n\xff\xfe\r\n".to_vec(),
                string(&long),
                string("good"),
            ]
            .concat(),
        ),
        _ => None,
    })
    .await;
    let (_, json) = vk.keys(json!({ "db": 0 })).await;
    assert_eq!(json["keys"], json!(["good"]));
    assert_eq!(json["unshown"], 2);
}

#[tokio::test]
async fn each_type_of_key_gives_its_value() {
    let vk = valkey(shop).await;
    for (key, ttl, value) in [
        (
            "name",
            Value::Null,
            json!({ "type": "string", "value": "Marsa" }),
        ),
        (
            "cart",
            json!(120),
            json!({ "type": "hash", "fields": [["sku", "14"], ["qty", "2"]], "total": 2 }),
        ),
        (
            "jobs",
            Value::Null,
            json!({ "type": "list", "items": ["first", "second"], "total": 900 }),
        ),
        (
            "ids",
            Value::Null,
            json!({ "type": "set", "members": ["12"], "total": 1 }),
        ),
        (
            "top",
            Value::Null,
            json!({ "type": "zset", "members": [["omar", "12.5"]], "total": 1 }),
        ),
        (
            "log",
            Value::Null,
            json!({ "type": "other", "name": "stream" }),
        ),
        (
            "bloom",
            Value::Null,
            json!({ "type": "other", "name": "MBbloom--" }),
        ),
    ] {
        let (status, json) = vk.key(key).await;
        assert_eq!(status, StatusCode::OK, "{key}: {json}");
        assert_eq!(json, json!({ "ttl": ttl, "value": value }), "{key}");
    }
}

/// Every command of every route is a read of the closed list.
#[tokio::test]
async fn only_reads_are_ever_sent() {
    let vk = valkey(shop).await;
    vk.app.router.answer("GET", "/v1/db/sources", None).await;
    vk.keys(json!({ "db": 0 })).await;
    for key in ["name", "cart", "jobs", "ids", "top", "log", "gone"] {
        vk.key(key).await;
    }
    let allowed = [
        "SELECT", "DBSIZE", "SCAN", "TYPE", "TTL", "STRLEN", "GETRANGE", "LLEN", "LRANGE", "HLEN",
        "HSCAN", "SCARD", "SSCAN", "ZCARD", "ZRANGE",
    ];
    let seen = vk.seen.lock().unwrap();
    for (_, words) in seen.iter() {
        assert!(allowed.contains(&words[0].as_str()), "{words:?}");
    }
    for word in allowed {
        assert!(seen.iter().any(|(_, w)| w[0] == word), "{word} never sent");
    }
}

#[tokio::test]
async fn the_sources_list_the_databases_with_keys() {
    let db = AtomicUsize::new(0);
    let vk = valkey(move |words| match words {
        ["SELECT", number] => {
            db.store(number.parse().unwrap(), Ordering::SeqCst);
            Some(status("OK"))
        }
        ["DBSIZE"] => Some(integer(match db.load(Ordering::SeqCst) {
            1 => 34,
            9 => 2,
            _ => 0,
        })),
        _ => None,
    })
    .await;
    let (_, json) = vk.app.router.answer("GET", "/v1/db/sources", None).await;
    assert_eq!(json["valkey"]["state"], "running", "{json}");
    assert_eq!(
        json["valkey"]["databases"],
        json!([{ "db": 0, "keys": 0 }, { "db": 1, "keys": 34 }, { "db": 9, "keys": 2 }])
    );
}

#[tokio::test]
async fn a_key_that_is_gone_is_not_found() {
    // Gone before the first read, and gone between the type and the rest.
    let vk = valkey(|words| match words {
        ["TYPE", "late"] => Some(status("list")),
        ["TYPE", _] => Some(status("none")),
        ["LLEN", _] => Some(integer(0)),
        ["LRANGE", ..] => Some(strings(&[])),
        ["TTL", _] => Some(integer(-2)),
        _ => None,
    })
    .await;
    for key in ["early", "late"] {
        let (status, json) = vk.key(key).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{key}: {json}");
        assert_eq!(json["error"]["code"], "db_key_not_found");
    }
    // The connection stays: the server answered as it should.
    assert_eq!(vk.opened.load(Ordering::SeqCst), 1);
    assert_eq!(vk.closed.load(Ordering::SeqCst), 0);
}

/// A long string keeps its first 64 KiB, and a long element its first 8 KiB;
/// a collection its first 500 elements.
#[tokio::test]
async fn long_values_come_back_cut() {
    let long = "v".repeat(VALUE_BYTES + 10);
    let vk = valkey(move |words| match words {
        ["TYPE", "text"] => Some(status("string")),
        ["TYPE", _] => Some(status("set")),
        ["TTL", _] => Some(integer(-1)),
        // The server answers the range it was asked for.
        ["STRLEN", _] => Some(integer(3_000_000)),
        ["GETRANGE", ..] => Some(string(&long[..VALUE_BYTES])),
        ["SCARD", _] => Some(integer(700)),
        ["SSCAN", ..] => {
            let element = &long[..ELEMENT_BYTES + 5];
            let found: Vec<&str> = std::iter::once(element).chain(["m"; 600]).collect();
            Some(scanned("0", &found))
        }
        _ => None,
    })
    .await;
    let (_, json) = vk.key("text").await;
    let value = &json["value"]["value"];
    assert_eq!(value["bytes"], 3_000_000);
    assert_eq!(value["text"].as_str().unwrap().len(), VALUE_BYTES);
    let (_, json) = vk.key("members").await;
    let members = json["value"]["members"].as_array().unwrap();
    assert_eq!(members.len(), KEY_ELEMENTS);
    assert_eq!(members[0]["bytes"], ELEMENT_BYTES + 5);
    assert_eq!(members[0]["text"].as_str().unwrap().len(), ELEMENT_BYTES);
    assert_eq!(json["value"]["total"], 700);
}

/// An answer that cannot be read, or is too large, or is not the command's,
/// is `db_unavailable`, and its connection is closed: the next request opens
/// a new one.
#[tokio::test]
async fn a_bad_answer_closes_the_connection() {
    for bad in [
        &b"?\r\n"[..],
        b"$-5\r\n",
        b"*1000000000\r\n",
        b"*1\r\n*1\r\n*1\r\n*1\r\n*1\r\n",
        b"$999999999999\r\n",
        b"+OK\r\n+OK\r\n",
        // RESP, but not what `SCAN` answers.
        b":5\r\n",
        b"-ERR something broke\r\n",
    ] {
        let reply = bad.to_vec();
        let vk = valkey(move |words| match words {
            ["SCAN", ..] => Some(reply.clone()),
            _ => None,
        })
        .await;
        let shown = String::from_utf8_lossy(bad);
        let (status, json) = vk.keys(json!({ "db": 0 })).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{shown:?}: {json}");
        assert_eq!(json["error"]["code"], "db_unavailable", "{shown:?}");
        vk.until_closed(1).await;
        vk.keys(json!({ "db": 0 })).await;
        assert_eq!(vk.opened.load(Ordering::SeqCst), 2, "{shown:?}");
    }
}

#[tokio::test]
async fn an_answer_cut_by_a_closed_connection_is_unavailable() {
    let vk = valkey(|words| match words {
        ["SCAN", ..] => Some(hang_up(b"*2\r\n$1\r\n")),
        _ => None,
    })
    .await;
    let (status, json) = vk.keys(json!({ "db": 0 })).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{json}");
    assert_eq!(json["error"]["code"], "db_unavailable");
}

/// Half an answer, then silence: the request ends at the browser's time
/// limit, and the connection closes.
#[tokio::test]
async fn an_answer_that_never_ends_is_a_timeout() {
    let vk = valkey(|words| match words {
        ["SCAN", ..] => Some(b"*2\r\n$1\r\n".to_vec()),
        _ => None,
    })
    .await;
    let (status, json) = vk.keys(json!({ "db": 0 })).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{json}");
    assert_eq!(json["error"]["code"], "db_timeout");
    vk.until_closed(1).await;
}

#[tokio::test]
async fn a_password_on_valkey_is_a_refused_sign_in() {
    let vk = valkey(|_| Some(b"-NOAUTH Authentication required.\r\n".to_vec())).await;
    let (status, json) = vk.keys(json!({ "db": 0 })).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{json}");
    assert_eq!(json["error"]["code"], "db_sign_in_failed");
    assert_eq!(json["detail"], "NOAUTH Authentication required.");
    let (status, json) = vk.app.router.answer("GET", "/v1/db/sources", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["valkey"]["problem"], "db_sign_in_failed", "{json}");
}

/// One connection serves the requests; `close` and a stop of Valkey close it.
#[tokio::test]
async fn the_connection_is_reused_until_close_or_a_stop() {
    let vk = valkey(shop).await;
    vk.keys(json!({ "db": 0 })).await;
    vk.key("name").await;
    assert_eq!(vk.opened.load(Ordering::SeqCst), 1);
    vk.app.router.send("POST", "/v1/db/close", None).await;
    vk.until_closed(1).await;

    vk.key("name").await;
    assert_eq!(vk.opened.load(Ordering::SeqCst), 2);
    let (status, json) = vk.post("/v1/services/valkey/stop", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    vk.until_closed(2).await;
}

/// A server that restarted dropped the old connection: the next request
/// opens a new one and answers.
#[tokio::test]
async fn a_dead_connection_is_replaced() {
    let selects = AtomicUsize::new(0);
    let vk = valkey(move |words| match words {
        // The second `SELECT` finds the connection closed.
        ["SELECT", _] if selects.fetch_add(1, Ordering::SeqCst) == 1 => Some(hang_up(b"")),
        _ => shop(words),
    })
    .await;
    vk.keys(json!({ "db": 0 })).await;
    let (status, json) = vk.keys(json!({ "db": 0 })).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(vk.opened.load(Ordering::SeqCst), 2);
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The log has the database and the request, never a key's name, a value,
/// the search, or what Valkey said.
#[tokio::test]
async fn the_log_holds_no_key_or_value() {
    let vk = valkey(|words| match words {
        ["DBSIZE"] => Some(integer(1)),
        ["SCAN", ..] => Some(scanned("0", &["secret_key_x"])),
        ["TYPE", "broken"] => Some(b"-ERR secret_error_w\r\n".to_vec()),
        ["TYPE", _] => Some(status("string")),
        ["STRLEN", _] => Some(integer(14)),
        ["GETRANGE", ..] => Some(string("secret_value_y")),
        ["TTL", _] => Some(integer(-1)),
        _ => None,
    })
    .await;
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // nextest runs each test in its own process.
    tracing::subscriber::set_global_default(subscriber).unwrap();
    vk.keys(json!({ "db": 0, "pattern": "secret_pattern_z" }))
        .await;
    vk.key("secret_key_x").await;
    vk.key("broken").await;
    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("valkey/0"), "{log}");
    assert!(!log.contains("secret_"), "{log}");
}
