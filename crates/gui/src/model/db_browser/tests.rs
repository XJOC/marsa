use marsa_core::codes::DB_SIGN_IN_FAILED;
use marsa_core::types::{
    DbDatabase, DbServerInfo, DbSiteFile, DbTableInfo, DbTableKind, DbTableRef, ServiceStatus,
};

use super::*;
use crate::fixtures::connected;

fn server(server: DbServer, state: ServiceState, databases: &[&str]) -> DbServerInfo {
    DbServerInfo {
        server,
        version: "8.4.3".into(),
        state,
        databases: databases
            .iter()
            .map(|name| DbDatabase {
                name: (*name).into(),
            })
            .collect(),
        problem: None,
    }
}

fn sources() -> DbSources {
    DbSources {
        servers: vec![server(
            DbServer::Mysql,
            ServiceState::Running,
            &["app", "shop"],
        )],
        files: vec![DbSiteFile {
            site: "blog".into(),
            path: "database/database.sqlite".into(),
        }],
    }
}

fn shop() -> DbSource {
    DbSource::Server {
        server: DbServer::Mysql,
        database: "shop".into(),
    }
}

fn tables(names: &[&str]) -> DbTables {
    DbTables {
        tables: names
            .iter()
            .map(|name| DbTableInfo {
                table: DbTableRef {
                    schema: None,
                    name: (*name).into(),
                },
                kind: DbTableKind::Table,
                estimate: Some(12_400),
            })
            .collect(),
    }
}

/// Opens Databases and answers its read of the sources; the read of the
/// first source's tables comes back.
fn opened(model: &mut Model, sources: DbSources) -> Option<Command> {
    let Some(Command::Db(DbCommand::ReadSources { id })) = model
        .apply_action(Action::Navigate(Route::Databases))
        .command()
    else {
        panic!("no read of the sources");
    };
    model.update(Update::Db(DbUpdate::Sources {
        id,
        result: Ok(sources),
    }));
    model.next_command()
}

fn tables_id(command: Option<Command>) -> u64 {
    match command {
        Some(Command::Db(DbCommand::ReadTables { id, .. })) => id,
        other => panic!("not a read of tables: {other:?}"),
    }
}

#[test]
fn opening_reads_the_sources_then_the_first_ones_tables() {
    let mut model = connected();
    let read = opened(&mut model, sources());
    let app = DbSource::Server {
        server: DbServer::Mysql,
        database: "app".into(),
    };
    assert!(
        matches!(&read, Some(Command::Db(DbCommand::ReadTables { source, .. })) if *source == app)
    );
    assert_eq!(model.session.db.chosen, Some(app));
    assert_eq!(model.session.db.screen(), DbScreen::Loading);
    let id = tables_id(read);
    model.update(Update::Db(DbUpdate::Tables {
        id,
        result: Ok(tables(&["users", "posts"])),
    }));
    assert_eq!(model.session.db.screen(), DbScreen::Tables);
}

#[test]
fn an_answer_to_a_read_the_user_moved_on_from_is_dropped() {
    let mut model = connected();
    let first = tables_id(opened(&mut model, sources()));
    let second = tables_id(model.apply_action(Action::ChooseDbSource(shop())).command());
    model.update(Update::Db(DbUpdate::Tables {
        id: first,
        result: Ok(tables(&["old"])),
    }));
    assert!(model.session.db.tables.is_none());
    model.update(Update::Db(DbUpdate::Tables {
        id: second,
        result: Ok(tables(&["orders"])),
    }));
    let shown = model.session.db.shown_tables();
    assert_eq!(shown[0].table.name, "orders");
}

#[test]
fn the_filter_is_local_and_reads_nothing() {
    let mut model = connected();
    let id = tables_id(opened(&mut model, sources()));
    model.update(Update::Db(DbUpdate::Tables {
        id,
        result: Ok(tables(&["users", "posts", "user_roles"])),
    }));
    let effect = model.apply_action(Action::FilterDbTables("USER".into()));
    assert!(effect.is_none());
    let names: Vec<_> = model
        .session
        .db
        .shown_tables()
        .iter()
        .map(|info| info.table.name.clone())
        .collect();
    assert_eq!(names, ["users", "user_roles"]);
}

#[test]
fn a_stopped_server_and_a_refused_sign_in_say_so() {
    let mut model = connected();
    let stopped = DbSources {
        servers: vec![server(DbServer::Postgres, ServiceState::Stopped, &[])],
        files: Vec::new(),
    };
    assert!(opened(&mut model, stopped).is_none());
    assert_eq!(
        model.session.db.screen(),
        DbScreen::Stopped(DbServer::Postgres)
    );

    let mut model = connected();
    let mut refused = server(DbServer::Mysql, ServiceState::Running, &[]);
    refused.problem = Some(DB_SIGN_IN_FAILED.into());
    let sources = DbSources {
        servers: vec![refused],
        files: Vec::new(),
    };
    opened(&mut model, sources);
    assert_eq!(
        model.session.db.screen(),
        DbScreen::SignInFailed(DbServer::Mysql)
    );
    assert!(matches!(
        model.apply_action(Action::ReadDbSources).command(),
        Some(Command::Db(DbCommand::ReadSources { .. }))
    ));

    let mut model = connected();
    opened(&mut model, DbSources::default());
    assert_eq!(model.session.db.screen(), DbScreen::Empty);
}

#[test]
fn leaving_drops_the_lists_closes_and_keeps_the_choice() {
    let mut model = connected();
    let id = tables_id(opened(&mut model, sources()));
    model.update(Update::Db(DbUpdate::Tables {
        id,
        result: Ok(tables(&["users"])),
    }));
    let chosen = model.session.db.chosen.clone();
    model.apply_action(Action::Navigate(Route::Mail));
    assert!(model.session.db.sources.is_none());
    assert!(model.session.db.tables.is_none());
    assert_eq!(model.session.db.chosen, chosen);
    assert!(matches!(
        model.next_command(),
        Some(Command::Db(DbCommand::Close))
    ));
}

#[test]
fn a_server_that_starts_while_the_screen_is_open_reads_the_list_again() {
    let mut model = connected();
    opened(&mut model, sources());
    let mysql = ServiceId::Database(DbServer::Mysql.kind());
    apply(
        &mut model,
        Event::ServiceChanged(ServiceStatus {
            id: mysql,
            state: ServiceState::Stopped,
            pid: None,
            restarts: 0,
            since_ms: None,
        }),
    );
    assert!(matches!(
        model.next_command(),
        Some(Command::Db(DbCommand::ReadSources { .. }))
    ));
    // Not open: nothing is read.
    model.apply_action(Action::Navigate(Route::Home));
    model.next_command();
    apply(
        &mut model,
        Event::ServiceChanged(ServiceStatus {
            id: mysql,
            state: ServiceState::Running,
            pid: Some(1),
            restarts: 0,
            since_ms: None,
        }),
    );
    assert!(model.next_command().is_none());
}

#[test]
fn the_sidebar_item_shows_only_when_something_could_be_opened() {
    let model = connected();
    assert!(!model.has_databases());
}
