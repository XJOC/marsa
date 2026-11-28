use eframe::egui::{self, Role};

use super::*;
use crate::theme::LIGHT;
use crate::widgets::harness::Harness;

#[test]
fn a_long_name_is_cut_in_the_middle() {
    let name = "laravel_database_queues:default:delayed";
    let cut = middle(name, 26);
    assert_eq!(cut.chars().count(), 26);
    assert!(cut.starts_with("laravel_datab"), "{cut}");
    assert!(cut.ends_with("ault:delayed"), "{cut}");
    assert!(cut.contains('…'), "{cut}");
    assert_eq!(middle("cart", 26), "cart");
    assert_eq!(middle("a\nb\tc", 26), "a b c");
    // Characters, not bytes, and a list too narrow for an end.
    assert_eq!(middle(&"é".repeat(40), 10).chars().count(), 10);
    assert!(middle(name, 3).chars().count() <= 3);
}

#[test]
fn the_arrows_reach_the_key_before_and_after() {
    let names: Vec<String> = ["a", "b", "c"].map(String::from).into();
    assert_eq!(neighbour(&names, None, 1), Some("a"));
    assert_eq!(neighbour(&names, Some("a"), 1), Some("b"));
    assert_eq!(neighbour(&names, Some("b"), -1), Some("a"));
    assert_eq!(neighbour(&names, Some("a"), -1), None);
    assert_eq!(neighbour(&names, Some("c"), 1), None);
    // A key no longer listed: the list starts over.
    assert_eq!(neighbour(&names, Some("gone"), 1), Some("a"));
    assert_eq!(neighbour(&[], None, 1), None);
}

#[test]
fn each_shape_has_its_columns() {
    let names = |shape| {
        let columns = columns(&shape);
        let kinds = columns.iter().map(|c| c.kind).collect::<Vec<_>>();
        let names = columns.into_iter().map(|c| c.name).collect::<Vec<_>>();
        (names, kinds)
    };
    assert_eq!(names(KeyShape::Hash).0, ["Field", "Value"]);
    assert_eq!(
        names(KeyShape::List),
        (
            vec!["Index".to_owned(), "Value".to_owned()],
            vec![DbColumnKind::Number, DbColumnKind::Text]
        )
    );
    assert_eq!(names(KeyShape::Set).0, ["Member"]);
    assert_eq!(names(KeyShape::Zset).1[1], DbColumnKind::Number);
}

fn browser() -> KeyBrowser {
    let mut keys = KeyBrowser::default();
    keys.list = Some(KeyList {
        names: (0..5_000).map(|n| format!("laravel_cache:{n}")).collect(),
        cursor: 9,
        unshown: 0,
        total: 9_000,
    });
    keys
}

/// Each name is a link a screen reader hears whole, and only the rows in view
/// are made.
#[test]
fn the_list_names_its_keys_and_draws_only_the_rows_in_view() {
    let harness = Harness::new(egui::Theme::Light);
    let keys = browser();
    let tree = harness.accessibility(|ui| {
        show(ui, &LIGHT, &keys, &mut Vec::new());
    });
    let first = tree.node_with_label("laravel_cache:0").expect("a key");
    assert_eq!(first.role(), Role::Link);
    assert!(tree.node_with_label("laravel_cache:4999").is_none());
    assert!(tree.node_with_label(strings::FIND_KEY).is_some());
}

/// The same values draw in the dark theme, and nothing asks for a read by
/// itself.
#[test]
fn drawing_a_key_asks_for_nothing() {
    let harness = Harness::new(egui::Theme::Dark);
    let mut keys = browser();
    keys.open = Some(OpenKey {
        name: "laravel_cache:1".into(),
        value: Some(KeyValue {
            ttl: Some(60),
            shape: KeyShape::Hash,
            rows: vec![vec![
                DbCell::Text("a".into()),
                DbCell::Cut {
                    text: "b".into(),
                    bytes: 9_000,
                },
            ]],
            total: 1,
        }),
        current: (Some(0), 1),
        pane: true,
        ..OpenKey::default()
    });
    let mut actions = Vec::new();
    let tree = harness.accessibility(|ui| show(ui, &LIGHT, &keys, &mut actions));
    assert!(actions.is_empty(), "{actions:?}");
    assert!(
        tree.node_with_label(strings::DB_CLOSE_VALUE).is_some(),
        "the pane"
    );
}

/// A hash's value is named after its field; a field, and any other cell,
/// after its column.
#[test]
fn the_pane_of_a_hash_names_the_field() {
    let value = KeyValue {
        ttl: None,
        shape: KeyShape::Hash,
        rows: vec![vec![
            DbCell::Text("items".into()),
            DbCell::Text("[]".into()),
        ]],
        total: 1,
    };
    let columns = columns(&value.shape);
    let at = |column| OpenKey {
        current: (Some(0), column),
        ..OpenKey::default()
    };
    assert_eq!(title(&at(1), &value, &columns), "items");
    assert_eq!(title(&at(0), &value, &columns), "Field");
    for odd in ["f".repeat(41), "two\nlines".to_owned()] {
        let mut value = value.clone();
        value.rows[0][0] = DbCell::Text(odd);
        assert_eq!(title(&at(1), &value, &columns), "Value");
    }
}
