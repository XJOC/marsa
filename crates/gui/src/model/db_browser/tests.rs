use marsa_core::codes::DB_SIGN_IN_FAILED;
use marsa_core::types::{
    DbCell, DbColumnKind, DbDatabase, DbPage, DbPageColumn, DbRowsRequest, DbServerInfo,
    DbSiteFile, DbTableInfo, DbTableKind, DbTableRef, DbTotal, DbValue, ServiceStatus,
};

use super::*;
use crate::fixtures::connected;
use crate::model::QueryState;

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

    // Not stopped: it takes no connections yet, so there is nothing to start.
    let mut model = connected();
    let starting = DbSources {
        servers: vec![server(DbServer::Postgres, ServiceState::Starting, &[])],
        files: Vec::new(),
    };
    opened(&mut model, starting);
    assert_eq!(model.session.db.screen(), DbScreen::Loading);

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

fn users() -> DbTableRef {
    DbTableRef {
        schema: None,
        name: "users".into(),
    }
}

/// The screen open on `app`, its tables read, and `users` chosen: the read
/// of its first page comes back.
fn table_opened(model: &mut Model) -> DbRowsRequest {
    let id = tables_id(opened(model, sources()));
    model.update(Update::Db(DbUpdate::Tables {
        id,
        result: Ok(tables(&["users", "posts"])),
    }));
    let read = model
        .apply_action(Action::Db(DbAction::ChooseTable(users())))
        .command();
    rows_read(read).1
}

fn rows_read(command: Option<Command>) -> (u64, DbRowsRequest) {
    match command {
        Some(Command::Db(DbCommand::ReadRows { id, request })) => (id, request),
        other => panic!("not a read of rows: {other:?}"),
    }
}

/// A page of `rows` rows from `offset`, of `total` (exact or not); `id`
/// is the key, `bio` a cut text.
fn page(offset: u64, rows: u64, total: u64, exact: bool) -> DbPage {
    let column = |name: &str, kind| DbPageColumn {
        name: name.into(),
        kind,
    };
    DbPage {
        columns: vec![
            column("id", DbColumnKind::Number),
            column("bio", DbColumnKind::Text),
        ],
        key: vec![0],
        rows: (offset..offset + rows)
            .map(|id| {
                vec![
                    DbCell::Text(id.to_string()),
                    DbCell::Cut {
                        text: "Long".into(),
                        bytes: 90_000,
                    },
                ]
            })
            .collect(),
        offset,
        total: DbTotal { rows: total, exact },
    }
}

fn answer_rows(model: &mut Model, id: u64, page: DbPage) {
    model.update(Update::Db(DbUpdate::Rows {
        id,
        result: Ok(page),
    }));
}

fn open(model: &Model) -> &OpenTable {
    model.session.db.open.as_ref().expect("a table is open")
}

/// Reads the next read of rows, from an action.
fn act(model: &mut Model, action: DbAction) -> Option<Command> {
    model.apply_action(Action::Db(action)).command()
}

#[test]
fn opening_a_table_reads_its_first_page_with_the_count() {
    let mut model = connected();
    let request = table_opened(&mut model);
    assert_eq!(request.offset, 0);
    assert!(request.count);
    assert_eq!(request.sort, None);
    assert_eq!(request.table, users());
    let id = model.session.db.rows_id.expect("a read");
    answer_rows(&mut model, id, page(0, 500, 1_204, true));
    let open = open(&model);
    assert_eq!(open.page.as_ref().map(|p| p.rows.len()), Some(500));
    assert_eq!(open.current, (Some(0), 0));
    assert_eq!(open.serial, 1);
    assert_eq!(open.loading, None);
}

#[test]
fn the_next_page_keeps_the_old_rows_until_it_comes_then_replaces_them() {
    let mut model = connected();
    table_opened(&mut model);
    let id = model.session.db.rows_id.expect("a read");
    answer_rows(&mut model, id, page(0, 500, 1_204, true));
    assert_eq!(open(&model).previous_offset(), None, "the first page");
    act(
        &mut model,
        DbAction::Current {
            row: Some(9),
            column: 1,
        },
    );

    let (id, request) = rows_read(act(&mut model, DbAction::NextPage));
    assert_eq!(request.offset, 500);
    assert!(!request.count, "a page reads no count");
    assert_eq!(open(&model).loading, Some(500));
    assert_eq!(open(&model).page.as_ref().map(|p| p.offset), Some(0));

    answer_rows(&mut model, id, page(500, 500, 1_204, true));
    assert_eq!(open(&model).current, (Some(0), 1), "back to the first row");
    assert_eq!(open(&model).serial, 2);

    let (id, request) = rows_read(act(&mut model, DbAction::NextPage));
    assert_eq!(request.offset, 1_000);
    answer_rows(&mut model, id, page(1_000, 204, 1_204, true));
    assert_eq!(open(&model).next_offset(), None, "the last page");
    assert!(act(&mut model, DbAction::NextPage).is_none());

    let (_, request) = rows_read(act(&mut model, DbAction::PreviousPage));
    assert_eq!(request.offset, 500);
}

#[test]
fn with_an_estimate_a_full_page_has_a_next_one() {
    let mut model = connected();
    table_opened(&mut model);
    let id = model.session.db.rows_id.expect("a read");
    answer_rows(&mut model, id, page(0, 500, 400, false));
    assert_eq!(open(&model).next_offset(), Some(500));
}

#[test]
fn a_header_sorts_up_then_down_then_not_and_each_reads_the_first_page() {
    let mut model = connected();
    table_opened(&mut model);
    let id = model.session.db.rows_id.expect("a read");
    answer_rows(&mut model, id, page(0, 500, 1_204, true));
    let mut sorts = Vec::new();
    for _ in 0..3 {
        let (_, request) = rows_read(act(&mut model, DbAction::Sort(1)));
        assert_eq!(request.offset, 0);
        sorts.push(request.sort.map(|sort| (sort.column, sort.descending)));
    }
    assert_eq!(
        sorts,
        [
            Some(("bio".into(), false)),
            Some(("bio".into(), true)),
            None
        ]
    );
}

#[test]
fn an_answer_to_a_page_the_user_moved_on_from_is_dropped() {
    let mut model = connected();
    table_opened(&mut model);
    let first = model.session.db.rows_id.expect("a read");
    answer_rows(&mut model, first, page(0, 500, 1_204, true));
    let (old, _) = rows_read(act(&mut model, DbAction::NextPage));
    let (new, _) = rows_read(act(&mut model, DbAction::Sort(0)));
    answer_rows(&mut model, old, page(500, 500, 1_204, true));
    assert_eq!(open(&model).page.as_ref().map(|p| p.offset), Some(0));
    answer_rows(&mut model, new, page(0, 500, 1_204, true));
    assert_eq!(open(&model).serial, 2);
}

#[test]
fn the_pane_on_a_cut_cell_reads_the_whole_value_by_the_key_and_copy_takes_it() {
    let mut model = connected();
    table_opened(&mut model);
    let id = model.session.db.rows_id.expect("a read");
    answer_rows(&mut model, id, page(0, 500, 1_204, true));
    act(
        &mut model,
        DbAction::Current {
            row: Some(3),
            column: 1,
        },
    );
    let Some(Command::Db(DbCommand::ReadValue { id, request })) =
        act(&mut model, DbAction::TogglePane)
    else {
        panic!("no read of the value");
    };
    assert_eq!(request.column, "bio");
    assert_eq!(request.key, ["3"]);
    assert!(open(&model).reading_value());
    model.update(Update::Db(DbUpdate::Value {
        id,
        result: Ok(DbValue {
            cell: DbCell::Text("Long, and the rest".into()),
        }),
    }));
    assert!(!open(&model).reading_value());
    assert!(matches!(
        model.apply_action(Action::Db(DbAction::Copy)),
        Effect::CopyText(text) if text == "Long, and the rest"
    ));
    // A whole cell needs no read.
    assert!(
        act(
            &mut model,
            DbAction::Current {
                row: Some(3),
                column: 0
            }
        )
        .is_none()
    );
    assert!(matches!(
        model.apply_action(Action::Db(DbAction::Copy)),
        Effect::CopyText(text) if text == "3"
    ));
    act(&mut model, DbAction::ClosePane);
    assert!(!open(&model).pane);
}

#[test]
fn without_a_primary_key_the_pane_shows_the_cut_text() {
    let mut model = connected();
    table_opened(&mut model);
    let id = model.session.db.rows_id.expect("a read");
    let mut no_key = page(0, 10, 10, true);
    no_key.key.clear();
    answer_rows(&mut model, id, no_key);
    act(
        &mut model,
        DbAction::Current {
            row: Some(0),
            column: 1,
        },
    );
    assert!(act(&mut model, DbAction::TogglePane).is_none());
    assert!(matches!(
        open(&model).pane_cell(),
        Some(DbCell::Cut { text, .. }) if text == "Long"
    ));
}

#[test]
fn leaving_drops_the_page_and_coming_back_reads_it_again_with_the_sort() {
    let mut model = connected();
    table_opened(&mut model);
    let id = model.session.db.rows_id.expect("a read");
    answer_rows(&mut model, id, page(0, 500, 1_204, true));
    act(&mut model, DbAction::Sort(1));
    model.apply_action(Action::Navigate(Route::Mail));
    assert!(open(&model).page.is_none());
    assert_eq!(open(&model).table, users());

    let id = tables_id(opened(&mut model, sources()));
    model.update(Update::Db(DbUpdate::Tables {
        id,
        result: Ok(tables(&["users"])),
    }));
    let (_, request) = rows_read(model.next_command());
    assert_eq!(request.sort.map(|sort| sort.column), Some("bio".into()));
}

#[test]
fn refresh_reads_the_same_page_with_the_count_and_columns_read_once() {
    let mut model = connected();
    table_opened(&mut model);
    let id = model.session.db.rows_id.expect("a read");
    answer_rows(&mut model, id, page(0, 500, 1_204, true));
    let (id, _) = rows_read(act(&mut model, DbAction::NextPage));
    answer_rows(&mut model, id, page(500, 500, 1_204, true));
    let (_, request) = rows_read(act(&mut model, DbAction::Refresh));
    assert_eq!(request.offset, 500);
    assert!(request.count);

    let Some(Command::Db(DbCommand::ReadTable { id, table, .. })) =
        act(&mut model, DbAction::Tab(DbTab::Columns))
    else {
        panic!("no read of the columns");
    };
    assert_eq!(table, users());
    model.update(Update::Db(DbUpdate::Table {
        id,
        result: Ok(marsa_core::types::DbTable {
            columns: Vec::new(),
        }),
    }));
    act(&mut model, DbAction::Tab(DbTab::Rows));
    assert!(act(&mut model, DbAction::Tab(DbTab::Columns)).is_none());
}

#[test]
fn another_source_closes_the_table() {
    let mut model = connected();
    table_opened(&mut model);
    model.apply_action(Action::ChooseDbSource(shop()));
    assert!(model.session.db.open.is_none());
}

fn run(model: &mut Model, sql: &str) -> (u64, marsa_core::types::DbQueryRequest) {
    match act(model, DbAction::Run(sql.into())) {
        Some(Command::Db(DbCommand::RunQuery { id, request })) => (id, request),
        other => panic!("not a run: {other:?}"),
    }
}

fn started(model: &mut Model, id: u64, job: u64) {
    model.update(Update::Db(DbUpdate::QueryStarted {
        id,
        result: Ok(job),
    }));
}

fn ended(
    model: &mut Model,
    id: u64,
    result: Result<marsa_core::types::DbResult, (String, Option<String>)>,
) {
    model.update(Update::Db(DbUpdate::Query { id, result }));
}

fn state(model: &Model) -> QueryState {
    model
        .session
        .db
        .query
        .as_ref()
        .expect("a run")
        .state
        .clone()
}

fn one_row() -> marsa_core::types::DbResult {
    marsa_core::types::DbResult {
        columns: Vec::new(),
        rows: vec![vec![DbCell::Text("1".into())]],
        cut: false,
        elapsed_ms: 4,
    }
}

#[test]
fn a_statement_runs_for_the_chosen_source_one_at_a_time() {
    let mut model = connected();
    tables_id(opened(&mut model, sources()));
    act(&mut model, DbAction::View(DbView::Sql));
    assert_eq!(model.session.db.view, DbView::Sql);
    let (id, request) = run(&mut model, "select 1");
    assert_eq!(request.sql, "select 1");
    assert_eq!(model.session.db.chosen.as_ref(), Some(&request.source));
    assert!(
        act(&mut model, DbAction::Run("select 2".into())).is_none(),
        "one at a time"
    );
    started(&mut model, id, 7);
    ended(&mut model, id, Ok(one_row()));
    assert_eq!(state(&model), QueryState::Done(one_row()));
    // Done: the next one may run.
    run(&mut model, "select 2");
    assert!(
        act(&mut model, DbAction::Run("   ".into())).is_none(),
        "nothing to run"
    );
}

#[test]
fn cancel_stops_the_job_and_says_cancelled_not_an_error() {
    let mut model = connected();
    tables_id(opened(&mut model, sources()));
    let (id, _) = run(&mut model, "select sleep(60)");
    started(&mut model, id, 7);
    assert!(matches!(
        act(&mut model, DbAction::Cancel),
        Some(Command::Db(DbCommand::CancelQuery { job: 7 }))
    ));
    assert_eq!(state(&model), QueryState::Cancelling);
    assert!(act(&mut model, DbAction::Cancel).is_none(), "once");
    let cancelled = (marsa_core::codes::DB_QUERY_CANCELLED.to_owned(), None);
    ended(&mut model, id, Err(cancelled));
    assert_eq!(state(&model), QueryState::Cancelled);
}

#[test]
fn a_cancel_before_the_job_started_goes_when_its_id_comes() {
    let mut model = connected();
    tables_id(opened(&mut model, sources()));
    let (id, _) = run(&mut model, "select sleep(60)");
    assert!(act(&mut model, DbAction::Cancel).is_none());
    started(&mut model, id, 9);
    assert!(matches!(
        model.next_command(),
        Some(Command::Db(DbCommand::CancelQuery { job: 9 }))
    ));
}

#[test]
fn a_refused_statement_keeps_the_servers_words_and_an_old_answer_is_dropped() {
    let mut model = connected();
    tables_id(opened(&mut model, sources()));
    let (id, _) = run(&mut model, "selec 1");
    let words = Some("You have an error in your SQL syntax".to_owned());
    ended(&mut model, id + 100, Ok(one_row()));
    assert_eq!(state(&model), QueryState::Running, "not this run's answer");
    ended(
        &mut model,
        id,
        Err(("db_query_failed".into(), words.clone())),
    );
    assert_eq!(
        state(&model),
        QueryState::Failed {
            code: "db_query_failed".into(),
            detail: words
        }
    );
}

#[test]
fn leaving_or_another_source_drops_the_result_and_a_table_goes_back_to_tables() {
    let mut model = connected();
    let id = tables_id(opened(&mut model, sources()));
    model.update(Update::Db(DbUpdate::Tables {
        id,
        result: Ok(tables(&["users"])),
    }));
    act(&mut model, DbAction::View(DbView::Sql));
    let (id, _) = run(&mut model, "select 1");
    ended(&mut model, id, Ok(one_row()));
    act(&mut model, DbAction::ChooseTable(users()));
    assert_eq!(model.session.db.view, DbView::Tables);
    assert!(
        model.session.db.query.is_some(),
        "the result stays on the screen"
    );
    model.apply_action(Action::Navigate(Route::Mail));
    assert!(model.session.db.query.is_none());
    assert_eq!(model.session.db.view, DbView::Tables);

    let mut model = connected();
    tables_id(opened(&mut model, sources()));
    run(&mut model, "select 1");
    model.apply_action(Action::ChooseDbSource(shop()));
    assert!(model.session.db.query.is_none());
}
