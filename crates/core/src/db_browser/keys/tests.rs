use super::super::resp::Decoder;
use super::*;

/// The words of an encoded command, read back as Valkey reads them.
fn words(command: KeyCommand<'_>) -> Vec<String> {
    let reply = Decoder::new(limits(PATTERN_BYTES + 1))
        .feed(&command.encode())
        .unwrap()
        .unwrap();
    bulks_reply(reply)
        .unwrap()
        .into_iter()
        .map(|bulk| String::from_utf8(bulk.bytes).unwrap())
        .collect()
}

#[test]
fn the_closed_list_as_sent() {
    let key = "queues:default";
    let scan = KeyCommand::Scan {
        cursor: 0,
        pattern: None,
        kind: None,
    };
    let narrow = KeyCommand::Scan {
        cursor: 176,
        pattern: Some("laravel*"),
        kind: Some(DbKeyKind::Zset),
    };
    for (command, sent) in [
        (KeyCommand::Select(1), "SELECT 1"),
        (KeyCommand::DbSize, "DBSIZE"),
        (scan, "SCAN 0 COUNT 100"),
        (narrow, "SCAN 176 MATCH laravel* COUNT 100 TYPE zset"),
        (KeyCommand::Type(key), "TYPE queues:default"),
        (KeyCommand::Ttl(key), "TTL queues:default"),
        (KeyCommand::StrLen(key), "STRLEN queues:default"),
        (KeyCommand::GetRange(key), "GETRANGE queues:default 0 65535"),
        (KeyCommand::LLen(key), "LLEN queues:default"),
        (KeyCommand::LRange(key), "LRANGE queues:default 0 499"),
        (KeyCommand::HLen(key), "HLEN queues:default"),
        (
            KeyCommand::HScan { key, cursor: 3 },
            "HSCAN queues:default 3 COUNT 100",
        ),
        (KeyCommand::SCard(key), "SCARD queues:default"),
        (
            KeyCommand::SScan { key, cursor: 0 },
            "SSCAN queues:default 0 COUNT 100",
        ),
        (KeyCommand::ZCard(key), "ZCARD queues:default"),
        (
            KeyCommand::ZRange(key),
            "ZRANGE queues:default 0 499 WITHSCORES",
        ),
    ] {
        assert_eq!(words(command).join(" "), sent);
    }
}

/// A pattern or a key with spaces, line breaks, and RESP's own characters is
/// one argument: nothing in it starts another word or another command.
#[test]
fn what_the_user_wrote_stays_one_argument() {
    let hostile = "a b\r\n*1\r\n$8\r\nFLUSHALL\r\n \"c\" é";
    let scan = KeyCommand::Scan {
        cursor: 0,
        pattern: Some(hostile),
        kind: None,
    };
    assert_eq!(words(scan), ["SCAN", "0", "MATCH", hostile, "COUNT", "100"]);
    let bytes = scan.encode();
    assert!(bytes.starts_with(b"*6\r\n"), "six words");
    for command in [
        KeyCommand::Type(hostile),
        KeyCommand::GetRange(hostile),
        KeyCommand::HScan {
            key: hostile,
            cursor: 0,
        },
    ] {
        assert_eq!(words(command)[1], hostile);
    }
}

#[test]
fn each_reply_may_keep_what_its_command_reads() {
    assert_eq!(KeyCommand::GetRange("k").limits().keep, VALUE_BYTES);
    assert_eq!(KeyCommand::LRange("k").limits().keep, ELEMENT_BYTES);
    let scan = KeyCommand::Scan {
        cursor: 0,
        pattern: None,
        kind: None,
    };
    assert_eq!(scan.limits().keep, KEY_BYTES + 1);
    // A whole page of a hash of long values fits what may be kept.
    let limits = KeyCommand::HScan {
        key: "k",
        cursor: 0,
    }
    .limits();
    assert!(limits.kept >= 2 * KEY_ELEMENTS * ELEMENT_BYTES);
    assert!(limits.items >= 2 * KEY_ELEMENTS as u64);
}

#[test]
fn the_search_field_finds_what_contains_the_text() {
    assert_eq!(key_pattern(""), None);
    assert_eq!(key_pattern("cache").as_deref(), Some("*cache*"));
    assert_eq!(
        key_pattern(r"App\Models").as_deref(),
        Some(r"*App\\Models*")
    );
    // A pattern stays as typed.
    for pattern in ["laravel*", "user:?", "job:[ab]", r"a\*b"] {
        assert_eq!(key_pattern(pattern).as_deref(), Some(pattern));
    }
}

#[test]
fn a_cut_string_keeps_whole_characters_and_its_length() {
    let bulk = |bytes: &[u8], len| Bulk {
        bytes: bytes.to_vec(),
        len,
    };
    let cut = |text: &str, bytes| DbCell::Cut {
        text: text.into(),
        bytes,
    };
    assert_eq!(bulk_cell(&bulk(b"", 0)), DbCell::Text(String::new()));
    assert_eq!(bulk_cell(&bulk(b"job", 3)), DbCell::Text("job".into()));
    assert_eq!(bulk_cell(&bulk(b"job", 900)), cut("job", 900));
    // The cut fell inside "é" (c3 a9).
    assert_eq!(bulk_cell(&bulk(b"ab\xc3", 4)), cut("ab", 4));
    // Not UTF-8, whole or cut: its length only.
    for len in [3, 900] {
        let binary = bulk_cell(&bulk(b"a\xffb", len));
        assert_eq!(binary, DbCell::Binary { bytes: len });
    }
    // A value that ends in half a character is binary, not cut.
    assert_eq!(bulk_cell(&bulk(b"ab\xc3", 3)), DbCell::Binary { bytes: 3 });
}

fn text(text: &str) -> Reply {
    Reply::Bulk(Bulk {
        bytes: text.into(),
        len: text.len() as u64,
    })
}

#[test]
fn the_answers_are_read_by_their_shape() {
    assert_eq!(count_reply(Reply::Integer(7)), Ok(7));
    let found = Reply::Array(vec![text("42"), Reply::Array(vec![text("a")])]);
    let (cursor, keys) = scan_reply(found).unwrap();
    assert_eq!((cursor, keys.len()), (42, 1));
    let items = bulks_reply(Reply::Array(vec![text("a"), text("1")])).unwrap();
    assert_eq!(pairs(items).unwrap().len(), 1);
}

#[test]
fn an_answer_of_another_shape_is_refused() {
    let status = || Reply::Status("OK".into());
    for reply in [Reply::Integer(-1), text("7"), status(), Reply::Null] {
        assert!(count_reply(reply).is_err());
    }
    for reply in [
        Reply::Null,
        Reply::Array(vec![Reply::Null]),
        Reply::Array(vec![Reply::Integer(1)]),
        text("a"),
    ] {
        assert!(bulks_reply(reply).is_err());
    }
    let empty = || Reply::Array(Vec::new());
    for reply in [
        status(),
        empty(),
        Reply::Array(vec![text("0")]),
        Reply::Array(vec![text("0"), empty(), empty()]),
        Reply::Array(vec![Reply::Integer(0), empty()]),
        Reply::Array(vec![text("x"), empty()]),
        Reply::Array(vec![text("-1"), empty()]),
        Reply::Array(vec![text("18446744073709551616"), empty()]),
        Reply::Array(vec![text("0"), text("a")]),
    ] {
        assert!(scan_reply(reply.clone()).is_err(), "{reply:?}");
    }
    assert!(pairs(vec![Bulk::default()]).is_err());
}
