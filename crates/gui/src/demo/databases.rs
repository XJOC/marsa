//! The Databases screen's fake data: MySQL with two databases and a site's
//! SQLite file, the tables of `shop` open; or MySQL stopped.

use marsa_core::types::{
    DbDatabase, DbServer, DbServerInfo, DbSiteFile, DbSource, DbSources, DbTableInfo, DbTableKind,
    DbTableRef, DbTables, ServiceState,
};

use super::Demo;
use crate::model::{Model, Route};

pub(super) fn fill(model: &mut Model, demo: Demo) {
    model.route = Route::Databases;
    let running = demo == Demo::Databases;
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
}
