//! The Dumps demos: what a Laravel project's `dump()` calls look like in
//! the window. The ages count back from the time the demo starts, so the
//! picture reads "just now" and "4 min ago".

use marsa_core::types::{Dump, DumpList, DumpsStatus};

use super::Demo;
use crate::model::{Model, Update};
use crate::screens::mail::now_ms;

/// The text of the newest demo dump, as `CliDumper` writes an Eloquent
/// model.
const USER: &str = "App\\Models\\User {#1893\n  #connection: \"mysql\"\n  #table: \"users\"\n  #attributes: array:5 [\n    \"id\" => 7\n    \"name\" => \"Omar\"\n    \"email\" => \"omar@example.test\"\n    \"created_at\" => \"2026-11-24 09:14:03\"\n    \"is_admin\" => true\n  ]\n  #original: array:5 [\n    \"id\" => 7\n    \"name\" => \"Omar\"\n    \"email\" => \"omar@example.test\"\n    \"created_at\" => \"2026-11-24 09:14:03\"\n    \"is_admin\" => true\n  ]\n  +timestamps: true\n}";

/// Each demo dump: minutes ago, the file and line, the request, its label,
/// and its text.
type Row = (u64, &'static str, u32, Option<&'static str>, &'static str);

const DUMPS: [Row; 6] = [
    (
        0,
        "/demo/Code/shop/app/Http/Controllers/UserController.php",
        24,
        Some("GET http://shop.test/users/7"),
        USER,
    ),
    (
        0,
        "/demo/Code/shop/routes/web.php",
        12,
        Some("GET http://shop.test/users/7"),
        "array:3 [\n  \"page\" => 1\n  \"per_page\" => 15\n  \"filters\" => array:1 [\n    \"active\" => true\n  ]\n]",
    ),
    (
        1,
        "/demo/Code/shop/app/Jobs/SendInvoice.php",
        41,
        Some("POST http://shop.test/orders"),
        "\"Invoice #1042 queued\"",
    ),
    (
        4,
        "/demo/Code/shop/app/Services/Cart.php",
        88,
        Some("GET http://shop.test/cart"),
        "App\\Services\\Cart {#2031\n  -items: array:2 [\n    0 => \"apple\"\n    1 => \"pear\"\n  ]\n  -total: 3.5\n  -coupon: null\n}",
    ),
    (
        22,
        "/demo/Code/blog/app/Http/Controllers/PostController.php",
        15,
        Some("GET http://blog.test/posts"),
        "12",
    ),
    (
        2 * 60,
        "/demo/Code/blog/routes/web.php",
        7,
        Some("GET http://blog.test/"),
        "true",
    ),
];

/// What the Dumps demos show beyond `Ok`: the dumps, and the dump server's
/// state.
pub(super) fn fill(model: &mut Model, demo: Demo) {
    let status = match demo {
        Demo::DumpsOff => DumpsStatus::Disabled,
        Demo::DumpsBusy => DumpsStatus::PortInUse,
        _ => DumpsStatus::Listening,
    };
    let dumps = match demo {
        Demo::Dumps => demo_dumps(),
        Demo::DumpsLong => vec![long_dump()],
        _ => Vec::new(),
    };
    let latest_id = dumps.first().map_or(0, |dump| dump.id);
    let count = dumps.len() as u64;
    // The two oldest were seen before: the others are new.
    let (new, seen) = dumps.split_at(dumps.len().saturating_sub(2));
    let seen_list = DumpList {
        status,
        latest_id: seen.first().map_or(0, |dump| dump.id),
        count: seen.len() as u64,
        dumps: seen.to_vec(),
    };
    model.update(Update::Dumps {
        list: seen_list,
        replace: true,
    });
    model.session.dumps.set_open(true);
    model.session.dumps.set_open(false);
    model.update(Update::Dumps {
        list: DumpList {
            status,
            latest_id,
            count,
            dumps: new.to_vec(),
        },
        replace: false,
    });
}

fn demo_dumps() -> Vec<Dump> {
    let now = now_ms();
    let len = DUMPS.len() as u64;
    DUMPS
        .iter()
        .enumerate()
        .map(|(index, (minutes, file, line, request, text))| {
            let (method, uri) = request
                .and_then(|request| request.split_once(' '))
                .map_or((None, None), |(method, uri)| {
                    (Some(method.to_owned()), Some(uri.to_owned()))
                });
            Dump {
                // Newest first: the first row has the biggest id.
                id: len - index as u64,
                received_at: now.saturating_sub(minutes * 60_000),
                source_file: Some((*file).to_owned()),
                source_line: Some(*line),
                uri,
                method,
                label: None,
                text: (*text).to_owned(),
                truncated: false,
            }
        })
        .collect()
}

/// One dump of 2 000 lines that was cut at its limit: the text scrolls in
/// both directions, and draws only the lines in view.
fn long_dump() -> Dump {
    let mut text = String::from("array:2000 [\n");
    for n in 0..2000 {
        text.push_str(&format!(
            "  {n} => \"row {n}: a value that is long enough to need scrolling sideways in a narrow window, with a path /demo/Code/shop/storage/app/{n}.json\"\n"
        ));
    }
    text.push(']');
    Dump {
        id: 1,
        received_at: now_ms(),
        source_file: Some("/demo/Code/shop/app/Console/Commands/Export.php".into()),
        source_line: Some(58),
        uri: Some("http://shop.test/export".into()),
        method: Some("POST".into()),
        label: Some("export".into()),
        text,
        truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{fill as fill_demo, parse};
    use crate::screens::dumps::{Body, view};

    fn demo(name: &str) -> Model {
        let mut model = Model::default();
        fill_demo(&mut model, parse(name).expect(name));
        model
    }

    #[test]
    fn the_dumps_demos_show_the_states_of_the_screen() {
        let model = demo("dumps");
        let shown = view(&model, now_ms());
        let Body::List(listed) = &shown.body else {
            panic!("a list: {:?}", shown.body);
        };
        assert_eq!(listed.dumps.len(), 6);
        assert_eq!(listed.shown, 0, "the newest is shown");
        assert!(listed.detail.text.starts_with("App\\Models\\User"));
        assert_eq!(shown.note, None);
        // Four came after the user last looked.
        assert_eq!(model.session.dumps.unseen(), 4);
        assert_eq!(view(&demo("dumps-empty"), now_ms()).body, Body::Empty);
        assert_eq!(view(&demo("dumps-off"), now_ms()).body, Body::Off);
        assert_eq!(view(&demo("dumps-busy"), now_ms()).body, Body::Busy);
        let long = demo("dumps-long");
        let shown = view(&long, now_ms());
        let Body::List(listed) = &shown.body else {
            panic!("a list: {:?}", shown.body);
        };
        assert!(listed.detail.cut);
        assert_eq!(listed.detail.text.lines().count(), 2002);
    }
}
