//! Valkey's keys in the database browser (ADR 0028, section 8): what the
//! window and `marsa db` ask `marsad`, and what it answers. A request names a
//! database by its number and a key by its name. It never carries a command,
//! so every request type refuses a field it does not know.

use serde::{Deserialize, Serialize};

use super::DbCell;

/// The types of key Valkey filters a search by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbKeyKind {
    String,
    Hash,
    List,
    Set,
    Zset,
    Stream,
}

impl DbKeyKind {
    /// In the order menus show them.
    pub const ALL: [Self; 6] = [
        Self::String,
        Self::Hash,
        Self::List,
        Self::Set,
        Self::Zset,
        Self::Stream,
    ];

    /// Valkey's word for the type, as `TYPE` answers it.
    pub fn word(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Hash => "hash",
            Self::List => "list",
            Self::Set => "set",
            Self::Zset => "zset",
            Self::Stream => "stream",
        }
    }

    /// The type Valkey calls `word`; `None` for a module's type.
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.word() == word)
    }
}

/// `POST /v1/db/keys`: some keys of a database, from where `cursor` says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbKeysRequest {
    /// Valkey's database number, from 0.
    pub db: u8,
    /// Only the keys this Valkey pattern matches, such as `laravel*`. At
    /// most `db_browser::PATTERN_BYTES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Only the keys of this type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<DbKeyKind>,
    /// 0 to start, else [`DbKeys::cursor`] of the answer before.
    #[serde(default)]
    pub cursor: u64,
}

/// The answer of `POST /v1/db/keys`. Valkey gives keys in no order, and one
/// may come twice while the database grows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbKeys {
    pub keys: Vec<String>,
    /// Where the next request goes on; 0 when no key is left.
    pub cursor: u64,
    /// Keys left out of `keys`: a name that is not UTF-8 text, or longer
    /// than `db_browser::KEY_BYTES`.
    pub unshown: u64,
    /// Every key of the database, whatever the pattern and the type.
    pub total: u64,
}

/// `POST /v1/db/key`: one key's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbKeyRequest {
    pub db: u8,
    pub key: String,
}

/// The answer of `POST /v1/db/key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbKeyDetail {
    /// The seconds the key has left when read; `None` when it does not
    /// expire.
    pub ttl: Option<u64>,
    pub value: DbKeyValue,
}

/// A key's value, by its type. Of a collection only the first
/// `db_browser::KEY_ELEMENTS` elements travel, each cut at
/// `db_browser::ELEMENT_BYTES`; `total` counts them all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DbKeyValue {
    /// Cut at `db_browser::VALUE_BYTES`.
    String {
        value: DbCell,
    },
    Hash {
        /// Each field with its value.
        fields: Vec<(DbCell, DbCell)>,
        total: u64,
    },
    List {
        items: Vec<DbCell>,
        total: u64,
    },
    Set {
        members: Vec<DbCell>,
        total: u64,
    },
    Zset {
        /// Each member with its score, as Valkey writes it, lowest first.
        members: Vec<(DbCell, String)>,
        total: u64,
    },
    /// A type the browser does not read: a stream, or a module's type.
    Other {
        /// Valkey's word for it.
        name: String,
    },
}

#[cfg(test)]
mod db_keys_tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use std::fmt;

    fn round_trip<T>(value: &T, expected: Value)
    where
        T: Serialize + DeserializeOwned + PartialEq + fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), expected);
        let back: T = serde_json::from_value(expected).unwrap();
        assert_eq!(&back, value);
    }

    #[test]
    fn each_type_of_key_has_valkeys_word() {
        for kind in DbKeyKind::ALL {
            assert_eq!(DbKeyKind::from_word(kind.word()), Some(kind));
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.word()));
        }
        for word in ["none", "", "String", "ReJSON-RL"] {
            assert_eq!(DbKeyKind::from_word(word), None, "{word}");
        }
    }

    #[test]
    fn the_key_requests_round_trip() {
        round_trip(
            &DbKeysRequest {
                db: 1,
                pattern: Some("laravel*".into()),
                kind: Some(DbKeyKind::Zset),
                cursor: 17,
            },
            json!({ "db": 1, "pattern": "laravel*", "kind": "zset", "cursor": 17 }),
        );
        let first: DbKeysRequest = serde_json::from_value(json!({ "db": 0 })).unwrap();
        assert_eq!(first, DbKeysRequest::default());
        round_trip(
            &DbKeyRequest {
                db: 0,
                key: "queues:default".into(),
            },
            json!({ "db": 0, "key": "queues:default" }),
        );
    }

    /// A request names things; it can carry no command and no address.
    #[test]
    fn a_key_request_with_a_command_or_an_address_is_refused() {
        for field in ["command", "commands", "args", "host", "port", "password"] {
            let mut keys = json!({ "db": 0 });
            keys[field] = json!("FLUSHALL");
            assert!(
                serde_json::from_value::<DbKeysRequest>(keys).is_err(),
                "{field}"
            );
            let mut key = json!({ "db": 0, "key": "a" });
            key[field] = json!("FLUSHALL");
            assert!(
                serde_json::from_value::<DbKeyRequest>(key).is_err(),
                "{field}"
            );
        }
        assert!(serde_json::from_value::<DbKeysRequest>(json!({ "db": -1 })).is_err());
        assert!(serde_json::from_value::<DbKeysRequest>(json!({ "db": 256 })).is_err());
        let unknown = json!({ "db": 0, "kind": "module" });
        assert!(serde_json::from_value::<DbKeysRequest>(unknown).is_err());
    }

    #[test]
    fn each_shape_of_value_round_trips() {
        let text = |text: &str| DbCell::Text(text.into());
        let cut = DbCell::Cut {
            text: "ab".into(),
            bytes: 9000,
        };
        for (value, wire) in [
            (
                DbKeyValue::String { value: cut.clone() },
                json!({ "type": "string", "value": { "text": "ab", "bytes": 9000 } }),
            ),
            (
                DbKeyValue::Hash {
                    fields: vec![(text("a"), DbCell::Binary { bytes: 3 })],
                    total: 1,
                },
                json!({ "type": "hash", "fields": [["a", { "bytes": 3 }]], "total": 1 }),
            ),
            (
                DbKeyValue::List {
                    items: vec![text("job"), cut],
                    total: 700,
                },
                json!({ "type": "list", "items": ["job", { "text": "ab", "bytes": 9000 }],
                    "total": 700 }),
            ),
            (
                DbKeyValue::Set {
                    members: vec![text("12")],
                    total: 1,
                },
                json!({ "type": "set", "members": ["12"], "total": 1 }),
            ),
            (
                DbKeyValue::Zset {
                    members: vec![(text("omar"), "12.5".into())],
                    total: 1,
                },
                json!({ "type": "zset", "members": [["omar", "12.5"]], "total": 1 }),
            ),
            (
                DbKeyValue::Other {
                    name: "stream".into(),
                },
                json!({ "type": "other", "name": "stream" }),
            ),
        ] {
            let detail = DbKeyDetail {
                ttl: Some(60),
                value,
            };
            round_trip(&detail, json!({ "ttl": 60, "value": wire }));
        }
    }
}
