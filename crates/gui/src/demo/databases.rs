//! The Databases screen's fake data: MySQL with two databases and a site's
//! SQLite file, the tables of `shop` open; or MySQL stopped; or the rows of
//! `users`, a cell chosen and its value open.

use marsa_core::types::{
    DbCell, DbColumn, DbColumnKind, DbDatabase, DbKey, DbPage, DbPageColumn, DbServer,
    DbServerInfo, DbSiteFile, DbSource, DbSources, DbTable, DbTableInfo, DbTableKind, DbTableRef,
    DbTables, DbTotal, ServiceState,
};

use super::Demo;
use crate::model::{DbBrowserState, DbTab, DbView, Model, OpenTable, QueryRun, QueryState, Route};

pub(super) fn fill(model: &mut Model, demo: Demo) {
    model.route = Route::Databases;
    let running = demo != Demo::DatabasesStopped;
    let databases = ["app", "shop"]
        .iter()
        .filter(|_| running)
        .map(|name| DbDatabase {
            name: (*name).into(),
        })
        .collect();
    let db = &mut model.session.db;
    db.sources = Some(DbSources {
        servers: vec![DbServerInfo {
            server: DbServer::Mysql,
            version: "8.4.3".into(),
            state: if running {
                ServiceState::Running
            } else {
                ServiceState::Stopped
            },
            databases,
            problem: None,
        }],
        files: vec![DbSiteFile {
            site: "blog".into(),
            path: "database/database.sqlite".into(),
        }],
        valkey: None,
    });
    if !running {
        db.chosen = Some(DbSource::Server {
            server: DbServer::Mysql,
            database: "shop".into(),
        });
        return;
    }
    db.chosen = Some(DbSource::Server {
        server: DbServer::Mysql,
        database: "shop".into(),
    });
    let table = |name: &str, kind, estimate| DbTableInfo {
        table: DbTableRef {
            schema: None,
            name: name.into(),
        },
        kind,
        estimate,
    };
    db.tables = Some(DbTables {
        tables: vec![
            table("cache", DbTableKind::Table, Some(0)),
            table("customers", DbTableKind::Table, Some(1_240)),
            table("migrations", DbTableKind::Table, Some(24)),
            table("order_items", DbTableKind::Table, Some(48_210)),
            table("orders", DbTableKind::Table, Some(12_408)),
            table("products", DbTableKind::Table, Some(312)),
            table("sessions", DbTableKind::Table, Some(88)),
            table("top_customers", DbTableKind::View, None),
            table("users", DbTableKind::Table, Some(1_250_000)),
        ],
    });
    sql(db, demo);
    if matches!(demo, Demo::DatabasesRows | Demo::DatabasesColumns) {
        let mut open = users_open();
        if demo == Demo::DatabasesColumns {
            open.tab = DbTab::Columns;
            open.columns = Some(users_columns());
        }
        db.open = Some(open);
    }
}

/// `users`, sorted by `id`, its first page; the `settings` of the first row
/// open in the value pane.
fn users_open() -> OpenTable {
    let column = |name: &str, kind| DbPageColumn {
        name: name.into(),
        kind,
    };
    let people = [
        ("Omar Haddad", "omar", Some("2026-03-02 09:14:55"), "1"),
        ("Lina Saleh", "lina", Some("2026-03-02 10:01:12"), "0"),
        ("Sam Carter", "sam", None, "0"),
        ("Noor Aziz", "noor", Some("2026-03-04 18:40:03"), "0"),
        ("Maya Stone", "maya", Some("2026-03-05 07:22:41"), "0"),
        ("Yusuf Karim", "yusuf", None, "0"),
        ("Ella Brooks", "ella", Some("2026-03-09 13:05:27"), "0"),
        ("Hana Mori", "hana", Some("2026-03-11 21:48:09"), "1"),
        ("Adam Reyes", "adam", Some("2026-03-12 08:30:00"), "0"),
        ("Sara Nasser", "sara", Some("2026-03-12 16:12:44"), "0"),
        ("Leo Marsh", "leo", None, "0"),
        ("Dana Khoury", "dana", Some("2026-03-15 11:11:11"), "0"),
        ("Ivan Petrov", "ivan", Some("2026-03-16 19:02:36"), "0"),
        ("Rania Fares", "rania", Some("2026-03-18 06:55:20"), "0"),
    ];
    let settings = "{\"theme\":\"dark\",\"locale\":\"ar\",\"digest\":{\"weekly\":true,\"hour\":8},\
        \"beta\":[\"grid\",\"sql\",\"keys\"],\"shortcuts\":{\"open\":\"cmd+o\",\"run\":\"cmd+enter\"},\
        \"seen_tips\":[1,2,3,5,8,13],\"timezone\":\"Asia/Riyadh\"}";
    let text = |text: &str| DbCell::Text(text.into());
    let rows = people
        .iter()
        .enumerate()
        .map(|(index, (name, mail, verified, admin))| {
            vec![
                text(&(index + 1).to_string()),
                text(name),
                text(&format!("{mail}@example.test")),
                verified.map_or(DbCell::Null, text),
                text(admin),
                match index % 3 {
                    0 => text(settings),
                    1 => text("{\"theme\":\"light\",\"locale\":\"en\"}"),
                    _ => DbCell::Null,
                },
                text(&format!("2026-03-{:02} 09:14:55", index + 2)),
            ]
        })
        .collect();
    let mut open = OpenTable::closed(DbTableRef {
        schema: None,
        name: "users".into(),
    });
    open.page = Some(DbPage {
        columns: vec![
            column("id", DbColumnKind::Number),
            column("name", DbColumnKind::Text),
            column("email", DbColumnKind::Text),
            column("email_verified_at", DbColumnKind::Time),
            column("is_admin", DbColumnKind::Number),
            column("settings", DbColumnKind::Text),
            column("created_at", DbColumnKind::Time),
        ],
        key: vec![0],
        rows,
        offset: 0,
        total: DbTotal {
            rows: 12_408,
            exact: true,
        },
    });
    open.serial = 1;
    open.sort = Some(("id".into(), false));
    open.current = (Some(0), 5);
    open.pane = true;
    open
}

/// The columns of `users`, as Laravel's first migration makes them.
fn users_columns() -> DbTable {
    let column = |name: &str, type_name: &str, nullable, default: Option<&str>, key| DbColumn {
        name: name.into(),
        type_name: type_name.into(),
        nullable,
        default: default.map(Into::into),
        key,
    };
    DbTable {
        columns: vec![
            column("id", "bigint unsigned", false, None, Some(DbKey::Primary)),
            column("name", "varchar(255)", false, None, None),
            column("email", "varchar(255)", false, None, Some(DbKey::Unique)),
            column("email_verified_at", "timestamp", true, None, None),
            column("is_admin", "tinyint(1)", false, Some("0"), None),
            column("settings", "json", true, None, None),
            column("created_at", "timestamp", true, None, None),
        ],
    }
}

/// The SQL view of `shop`: the orders of a week, as rows, as a statement
/// still running, or as one MySQL refused.
fn sql(db: &mut DbBrowserState, demo: Demo) {
    let state = match demo {
        Demo::DatabasesSql => {
            let column = |name: &str, kind| DbPageColumn {
                name: name.into(),
                kind,
            };
            let rows = (0..8_u32)
                .map(|n| {
                    vec![
                        DbCell::Text((1041 + n).to_string()),
                        DbCell::Text(
                            ["Omar Haddad", "Lina Saleh", "Sam Carter", "Noor Aziz"]
                                [n as usize % 4]
                                .into(),
                        ),
                        DbCell::Text(format!("{}.{:02}", 40 + n * 13, n * 7 % 100)),
                        DbCell::Text(format!("2026-11-{:02} 10:{:02}:00", 18 + n % 7, n * 7)),
                    ]
                })
                .collect();
            QueryState::Done(marsa_core::types::DbResult {
                columns: vec![
                    column("id", DbColumnKind::Number),
                    column("customer", DbColumnKind::Text),
                    column("total", DbColumnKind::Number),
                    column("created_at", DbColumnKind::Time),
                ],
                rows,
                cut: false,
                elapsed_ms: 4,
            })
        }
        Demo::DatabasesSqlRunning => QueryState::Running,
        Demo::DatabasesSqlRefused => QueryState::Failed {
            code: marsa_core::codes::DB_QUERY_FAILED.into(),
            detail: Some(
                "You have an error in your SQL syntax; check the manual that corresponds to \
                 your MySQL server version for the right syntax to use near 'FORM orders o\n\
                 JOIN users u ON u.id = o.user_id' at line 3"
                    .into(),
            ),
        },
        _ => return,
    };
    db.view = DbView::Sql;
    let refused = demo == Demo::DatabasesSqlRefused;
    db.demo_sql = Some(format!(
        "-- The orders of the last week, newest first.\n\
         SELECT o.id, u.name AS customer, o.total, o.created_at\n\
         {} orders o\n\
         JOIN users u ON u.id = o.user_id\n\
         WHERE o.created_at > '2026-11-18'\n\
         ORDER BY o.created_at DESC;",
        if refused { "FORM" } else { "FROM" }
    ));
    let started = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(12))
        .unwrap_or_else(std::time::Instant::now);
    db.query = Some(QueryRun {
        started,
        job: Some(1),
        state,
    });
}
