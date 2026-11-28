//! The fake data of Valkey's keys: a hash with a field's value open, a
//! string, a long list, no keys, a key that is gone, and Valkey stopped.

use marsa_core::types::{
    DbCell, DbKeyDetail, DbKeyValue, DbSources, DbValkeyDatabase, DbValkeyInfo, ServiceState,
};

use super::Demo;
use crate::model::{KeyBrowser, KeyList, Model, OpenKey, Route};

const KEYS: [&str; 8] = [
    "laravel_database_queues:default",
    "laravel_database_queues:default:delayed",
    "laravel_database_queues:default:notify",
    "laravel_database_user:12:cart",
    "laravel_database_online_users",
    "laravel_database_leaderboard",
    "laravel_database_settings",
    "laravel_database_reports:2026-11",
];

pub(super) fn fill(model: &mut Model, demo: Demo) {
    model.route = Route::Databases;
    let runs = demo != Demo::KeysStopped;
    let database = |db, keys| DbValkeyDatabase { db, keys };
    let empty = demo == Demo::KeysEmpty;
    model.session.db.sources = Some(DbSources {
        servers: Vec::new(),
        files: Vec::new(),
        valkey: Some(DbValkeyInfo {
            version: "8.1.11".into(),
            state: if runs {
                ServiceState::Running
            } else {
                ServiceState::Stopped
            },
            databases: match (runs, empty) {
                (false, _) => Vec::new(),
                (true, true) => vec![database(0, 0)],
                (true, false) => vec![database(0, 212), database(1, 34)],
            },
            problem: None,
        }),
    });
    let mut browser = KeyBrowser::default();
    if runs {
        let names: &[&str] = if empty { &[] } else { &KEYS };
        browser.list = Some(KeyList {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            cursor: if empty { 0 } else { 176 },
            unshown: 0,
            total: if empty { 0 } else { 212 },
        });
        browser.open = open(demo);
    }
    model.session.db.keys = Some(browser);
}

/// The chosen key of `demo`, if it shows one.
fn open(demo: Demo) -> Option<OpenKey> {
    let text = |text: &str| DbCell::Text(text.into());
    let (name, value, ttl) = match demo {
        Demo::Keys => (
            KEYS[3],
            DbKeyValue::Hash {
                fields: [
                    ("currency", "SAR"),
                    ("items", "[{\"id\":14,\"qty\":2},{\"id\":91,\"qty\":1}]"),
                    ("total", "412.40"),
                    ("updated_at", "2026-11-28 09:14:55"),
                ]
                .map(|(field, value)| (text(field), text(value)))
                .into(),
                total: 4,
            },
            Some(7_140),
        ),
        Demo::KeysString => (
            KEYS[6],
            DbKeyValue::String {
                value: text(
                    "a:3:{s:5:\"theme\";s:4:\"dark\";s:6:\"locale\";s:2:\"ar\";\
                     s:6:\"digest\";b:1;}",
                ),
            },
            None,
        ),
        Demo::KeysList => (
            KEYS[0],
            DbKeyValue::List {
                items: (0..500)
                    .map(|n| {
                        text(&format!(
                            "{{\"uuid\":\"9b1d{n:03}\",\"displayName\":\"App\\\\Jobs\\\\SendInvoice\",\
                             \"job\":\"Illuminate\\\\Queue\\\\CallQueuedHandler@call\",\"attempts\":0}}"
                        ))
                    })
                    .collect(),
                total: 12_408,
            },
            None,
        ),
        Demo::KeysGone => {
            let mut open = OpenKey {
                name: KEYS[3].into(),
                ..OpenKey::default()
            };
            open.error = Some((marsa_core::codes::DB_KEY_NOT_FOUND.into(), None));
            return Some(open);
        }
        _ => return None,
    };
    let value = crate::model::KeyValue::from(DbKeyDetail { ttl, value });
    let (column, pane) = match demo {
        Demo::KeysString => (0, false),
        _ => (1, true),
    };
    let row = if demo == Demo::Keys { 1 } else { 0 };
    Some(OpenKey {
        name: name.into(),
        current: (Some(row), column),
        value: Some(value),
        serial: 1,
        pane,
        ..OpenKey::default()
    })
}
