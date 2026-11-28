use marsa_core::codes::{DB_KEY_NOT_FOUND, DB_SIGN_IN_FAILED};
use marsa_core::runtimes::Runtime;
use marsa_core::types::{DbSiteFile, DbSources, DbValkeyDatabase, RuntimeList, RuntimeVersions};

use super::*;
use crate::bridge::DbCommand;
use crate::fixtures::connected;
use crate::model::{Effect, Route};

fn valkey(state: ServiceState, databases: &[u8]) -> DbValkeyInfo {
    DbValkeyInfo {
        version: "8.1.11".into(),
        state,
        databases: databases
            .iter()
            .map(|db| DbValkeyDatabase { db: *db, keys: 3 })
            .collect(),
        problem: None,
    }
}

fn sources(info: DbValkeyInfo) -> DbSources {
    DbSources {
        servers: Vec::new(),
        files: Vec::new(),
        valkey: Some(info),
    }
}

fn running() -> DbSources {
    sources(valkey(ServiceState::Running, &[0, 1]))
}

fn sent(effect: Effect) -> Option<Command> {
    effect.command()
}

/// Opens Databases and answers its read of the sources.
fn opened(model: &mut Model, list: DbSources) -> Option<Command> {
    let Some(Command::Db(DbCommand::ReadSources { id })) =
        sent(model.apply_action(Action::Navigate(Route::Databases)))
    else {
        panic!("no read of the sources");
    };
    model.update(Update::Db(DbUpdate::Sources {
        id,
        result: Ok(list),
    }));
    model.next_command()
}

fn keys_read(command: Option<Command>) -> (u64, DbKeysRequest) {
    match command {
        Some(Command::Db(DbCommand::ReadKeys { id, request })) => (id, request),
        other => panic!("not a read of keys: {other:?}"),
    }
}

fn key_read(command: Option<Command>) -> (u64, DbKeyRequest) {
    match command {
        Some(Command::Db(DbCommand::ReadKey { id, request })) => (id, request),
        other => panic!("not a read of a key: {other:?}"),
    }
}

fn page(names: &[&str], cursor: u64) -> DbKeys {
    DbKeys {
        keys: names.iter().map(|name| (*name).to_owned()).collect(),
        cursor,
        unshown: 1,
        total: 40,
    }
}

fn answer_keys(model: &mut Model, id: u64, page: DbKeys) {
    model.update(Update::Db(DbUpdate::Keys {
        id,
        result: Ok(page),
    }));
}

fn answer_key(model: &mut Model, id: u64, value: DbKeyValue) {
    model.update(Update::Db(DbUpdate::Key {
        id,
        result: Ok(DbKeyDetail { ttl: None, value }),
    }));
}

fn act(model: &mut Model, action: KeysAction) -> Option<Command> {
    sent(model.apply_action(Action::Keys(action)))
}

fn browser(model: &Model) -> &KeyBrowser {
    model.session.db.keys.as_ref().expect("Valkey is chosen")
}

fn names(model: &Model) -> Vec<String> {
    browser(model).list.as_ref().expect("a list").names.clone()
}

/// With one key opened in a list of two.
fn with_open_key(model: &mut Model) {
    let (id, _) = keys_read(opened(model, running()));
    answer_keys(model, id, page(&["cart", "jobs"], 0));
    let (id, _) = key_read(act(model, KeysAction::Open("cart".into())));
    let fields = vec![
        (DbCell::Text("sku".into()), DbCell::Text("14".into())),
        (DbCell::Text("qty".into()), DbCell::Text("2".into())),
    ];
    answer_key(model, id, DbKeyValue::Hash { fields, total: 2 });
}

#[test]
fn with_only_valkey_its_database_0_opens() {
    let mut model = connected();
    let (id, request) = keys_read(opened(&mut model, running()));
    assert_eq!(request, DbKeysRequest::default());
    assert_eq!(model.session.db.screen(), DbScreen::Keys);
    assert!(browser(&model).loading);
    answer_keys(&mut model, id, page(&["a"], 0));
    assert!(!browser(&model).loading);
    assert_eq!(names(&model), ["a"]);
}

/// A SQL source stays the first choice; Valkey is chosen from the menu, and
/// a SQL source chosen again leaves its keys.
#[test]
fn a_database_of_the_menu_reads_its_keys() {
    let mut model = connected();
    let mut list = running();
    list.files = vec![DbSiteFile {
        site: "blog".into(),
        path: "database/database.sqlite".into(),
    }];
    opened(&mut model, list);
    assert!(model.session.db.keys.is_none());
    let (_, request) = keys_read(act(&mut model, KeysAction::Database(1)));
    assert_eq!(request.db, 1);
    assert!(model.session.db.chosen.is_none());
    let blog = marsa_core::types::DbSource::SiteFile {
        site: "blog".into(),
    };
    let read = sent(model.apply_action(Action::ChooseDbSource(blog.clone())));
    assert!(matches!(
        read,
        Some(Command::Db(DbCommand::ReadTables { .. }))
    ));
    assert!(model.session.db.keys.is_none());
    assert_eq!(model.session.db.chosen, Some(blog));
}

#[test]
fn the_search_reads_on_enter_and_more_keys_goes_on_with_it() {
    let mut model = connected();
    let (id, _) = keys_read(opened(&mut model, running()));
    answer_keys(&mut model, id, page(&["a", "b"], 7));
    assert!(act(&mut model, KeysAction::Search("cache".into())).is_none());
    let (id, request) = keys_read(act(&mut model, KeysAction::Submit));
    assert_eq!(request.pattern.as_deref(), Some("*cache*"));
    assert_eq!(request.cursor, 0);
    assert!(browser(&model).list.is_none());
    answer_keys(&mut model, id, page(&["cache:1"], 9));
    // The field changes, the next page still reads with what was sent.
    act(&mut model, KeysAction::Search("other".into()));
    let (id, request) = keys_read(act(&mut model, KeysAction::More));
    assert_eq!(request.pattern.as_deref(), Some("*cache*"));
    assert_eq!(request.cursor, 9);
    // One read at a time.
    assert!(act(&mut model, KeysAction::More).is_none());
    // The list stays in the names' order, and a key given twice is listed
    // once.
    answer_keys(&mut model, id, page(&["cache:1", "cache:0"], 0));
    assert_eq!(names(&model), ["cache:0", "cache:1"]);
    let list = browser(&model).list.as_ref().unwrap();
    assert_eq!((list.unshown, list.more()), (2, false));
    assert!(act(&mut model, KeysAction::More).is_none());

    let (_, request) = keys_read(act(&mut model, KeysAction::Kind(Some(DbKeyKind::List))));
    assert_eq!(request.kind, Some(DbKeyKind::List));
    assert_eq!(request.pattern.as_deref(), Some("*other*"));
}

#[test]
fn the_list_stops_at_its_limit() {
    let names: Vec<String> = (0..KEY_LIST_LIMIT + 3).map(|n| n.to_string()).collect();
    let mut list = KeyList::default();
    list.append(DbKeys {
        keys: names,
        cursor: 4,
        ..DbKeys::default()
    });
    assert_eq!(list.names.len(), KEY_LIST_LIMIT);
    assert!(list.full() && !list.more());
}

#[test]
fn an_answer_to_a_read_the_user_moved_on_from_is_dropped() {
    let mut model = connected();
    let (first, _) = keys_read(opened(&mut model, running()));
    let (second, _) = keys_read(act(&mut model, KeysAction::Database(1)));
    answer_keys(&mut model, first, page(&["old"], 0));
    assert!(browser(&model).list.is_none());
    answer_keys(&mut model, second, page(&["new", "next"], 0));
    let (first, _) = key_read(act(&mut model, KeysAction::Open("new".into())));
    key_read(act(&mut model, KeysAction::Open("next".into())));
    answer_key(&mut model, first, DbKeyValue::Other { name: "x".into() });
    assert!(browser(&model).open.as_ref().unwrap().value.is_none());
}

#[test]
fn the_value_s_grid_copies_the_current_cell() {
    let mut model = connected();
    with_open_key(&mut model);
    let open = browser(&model).open.as_ref().unwrap();
    // A new value starts at its first row, on the value's column.
    assert_eq!(open.current, (Some(0), 1));
    let rows = open.value.as_ref().unwrap().rows.len();
    assert_eq!((rows, open.serial), (2, 1));
    act(
        &mut model,
        KeysAction::Current {
            row: Some(1),
            column: 0,
        },
    );
    act(&mut model, KeysAction::TogglePane);
    assert!(browser(&model).open.as_ref().unwrap().pane);
    let effect = model.apply_action(Action::Keys(KeysAction::Copy));
    assert!(matches!(effect, Effect::CopyText(text) if text == "qty"));
    act(&mut model, KeysAction::ClosePane);
    assert!(!browser(&model).open.as_ref().unwrap().pane);
}

#[test]
fn each_value_gives_its_rows() {
    let text = |text: &str| DbCell::Text(text.into());
    let rows = |value: DbKeyValue| {
        let value = KeyValue::from(DbKeyDetail {
            ttl: Some(9),
            value,
        });
        (value.rows, value.total)
    };
    let list = DbKeyValue::List {
        items: vec![text("job")],
        total: 900,
    };
    assert_eq!(rows(list), (vec![vec![text("0"), text("job")]], 900));
    let set = DbKeyValue::Set {
        members: vec![text("12")],
        total: 1,
    };
    assert_eq!(rows(set), (vec![vec![text("12")]], 1));
    let zset = DbKeyValue::Zset {
        members: vec![(text("omar"), "12.5".into())],
        total: 1,
    };
    assert_eq!(rows(zset), (vec![vec![text("omar"), text("12.5")]], 1));
    let cut = DbCell::Cut {
        text: "ab".into(),
        bytes: 900,
    };
    let string = DbKeyValue::String { value: cut.clone() };
    assert_eq!(rows(string), (vec![vec![cut]], 900));
    let other = DbKeyValue::Other { name: "x".into() };
    assert_eq!(rows(other), (Vec::new(), 0));
}

#[test]
fn a_key_that_is_gone_and_a_failed_read_say_so() {
    let mut model = connected();
    with_open_key(&mut model);
    let read = act(&mut model, KeysAction::Refresh);
    let (list, _) = keys_read(read);
    answer_keys(&mut model, list, page(&["jobs"], 0));
    let (id, request) = key_read(model.next_command());
    assert_eq!(request.key, "cart");
    model.update(Update::Db(DbUpdate::Key {
        id,
        result: Err((DB_KEY_NOT_FOUND.into(), None)),
    }));
    let open = browser(&model).open.as_ref().unwrap();
    assert!(open.value.is_none());
    assert_eq!(open.error.as_ref().unwrap().0, DB_KEY_NOT_FOUND);

    let (id, _) = keys_read(act(&mut model, KeysAction::Submit));
    let error = ("db_unavailable".to_owned(), Some("LOADING".to_owned()));
    model.update(Update::Db(DbUpdate::Keys {
        id,
        result: Err(error.clone()),
    }));
    assert_eq!(browser(&model).error, Some(error));
}

#[test]
fn leaving_drops_what_was_read_and_a_return_reads_it_again() {
    let mut model = connected();
    with_open_key(&mut model);
    act(&mut model, KeysAction::Search("ca".into()));
    let close = sent(model.apply_action(Action::Navigate(Route::Home)));
    let close = close.or_else(|| model.next_command());
    assert!(matches!(close, Some(Command::Db(DbCommand::Close))));
    let keys = browser(&model);
    assert!(keys.list.is_none());
    assert_eq!(keys.search, "ca");
    let open = keys.open.as_ref().unwrap();
    assert_eq!((open.name.as_str(), open.value.is_none()), ("cart", true));

    let (id, request) = keys_read(opened(&mut model, running()));
    assert_eq!(request.pattern, None, "the search was never sent");
    assert!(!browser(&model).narrowed());
    answer_keys(&mut model, id, page(&["cart"], 0));
    let (_, request) = key_read(model.next_command());
    assert_eq!(request.key, "cart");
}

#[test]
fn a_stopped_valkey_and_a_refused_sign_in_say_so() {
    let mut model = connected();
    let stopped = sources(valkey(ServiceState::Stopped, &[]));
    assert!(opened(&mut model, stopped).is_none());
    assert_eq!(model.session.db.screen(), DbScreen::KeysStopped);
    assert!(act(&mut model, KeysAction::Submit).is_none());

    let mut model = connected();
    let starting = sources(valkey(ServiceState::Starting, &[]));
    opened(&mut model, starting);
    assert_eq!(model.session.db.screen(), DbScreen::Loading);

    let mut model = connected();
    let mut info = valkey(ServiceState::Running, &[]);
    info.problem = Some(DB_SIGN_IN_FAILED.into());
    opened(&mut model, sources(info));
    assert_eq!(model.session.db.screen(), DbScreen::KeysSignInFailed);

    let mut model = connected();
    let mut info = valkey(ServiceState::Running, &[]);
    info.problem = Some("db_unavailable".into());
    opened(&mut model, sources(info));
    assert_eq!(
        model.session.db.screen(),
        DbScreen::Failed("db_unavailable".into())
    );
}

/// Valkey stops while its keys show: they go; it runs again: they are read.
#[test]
fn a_server_that_stops_and_starts_again() {
    let mut model = connected();
    with_open_key(&mut model);
    let reread = |model: &mut Model, list: DbSources| {
        model.db_sources_changed();
        let Some(Command::Db(DbCommand::ReadSources { id })) = model.next_command() else {
            panic!("no read of the sources");
        };
        model.update(Update::Db(DbUpdate::Sources {
            id,
            result: Ok(list),
        }));
        model.next_command()
    };
    let stopped = sources(valkey(ServiceState::Stopped, &[]));
    assert!(reread(&mut model, stopped).is_none());
    assert_eq!(model.session.db.screen(), DbScreen::KeysStopped);
    assert!(browser(&model).list.is_none());
    assert_eq!(browser(&model).open.as_ref().unwrap().name, "cart");
    keys_read(reread(&mut model, running()));
}

#[test]
fn an_installed_valkey_shows_the_sidebar_item() {
    let mut model = connected();
    assert!(!model.has_databases());
    model.update(Update::Runtimes(RuntimeList {
        runtimes: vec![RuntimeVersions {
            runtime: Runtime::Valkey,
            installed: vec!["8.1.11".into()],
            default: None,
        }],
    }));
    assert!(model.has_databases());
}
