//! The demos of a Laravel site's own log: Logs on `Laravel · shop`, with an
//! error and its stack trace among the entries, as Laravel writes them.
//! `shop` and `billing-api` are the Laravel sites here, so the sources fit
//! the segmented control (`MARSA_GUI_ROUTE=logs:site:billing-api` shows a
//! site with no log yet).

use marsa_core::types::{ServiceState, SiteKind, SiteLogLine};

use super::{Demo, service};
use crate::model::{Action, LogLevel, LogSource, Model, Route, Update};

/// The lines of `shop`'s log, oldest first.
const LINES: [&str; 15] = [
    r#"[2026-11-25 09:58:12] local.INFO: User 7 signed in {"ip":"127.0.0.1"}"#,
    r#"[2026-11-25 10:01:47] local.INFO: Order 1041 paid {"total":"49.00"}"#,
    r#"[2026-11-25 10:02:40] local.WARNING: Slow query on orders (1.8 s) {"sql":"select * from `orders` where `user_id` = ?"}"#,
    r#"[2026-11-25 10:15:03] local.ERROR: Call to undefined method App\Models\Order::total() {"userId":7,"exception":"[object] (BadMethodCallException(code: 0): Call to undefined method App\\Models\\Order::total() at /demo/Code/shop/vendor/laravel/framework/src/Illuminate/Support/Traits/ForwardsCalls.php:67)"#,
    "[stacktrace]",
    r"#0 /demo/Code/shop/vendor/laravel/framework/src/Illuminate/Support/Traits/ForwardsCalls.php(36): Illuminate\Database\Eloquent\Model::throwBadMethodCallException()",
    r"#1 /demo/Code/shop/vendor/laravel/framework/src/Illuminate/Database/Eloquent/Model.php(2449): Illuminate\Database\Eloquent\Model->forwardCallTo()",
    r"#2 /demo/Code/shop/app/Http/Controllers/OrderController.php(28): Illuminate\Database\Eloquent\Model->__call()",
    r"#3 /demo/Code/shop/vendor/laravel/framework/src/Illuminate/Routing/Controller.php(54): App\Http\Controllers\OrderController->show()",
    "#4 {main}",
    r#""}"#,
    r#"[2026-11-25 10:15:09] local.INFO: Order 1042 shipped {"carrier":"post"}"#,
    "[2026-11-25 10:16:30] local.WARNING: Mail queue is 120 jobs behind",
    r#"[2026-11-25 10:17:02] local.INFO: User 9 signed in {"ip":"127.0.0.1"}"#,
    "[2026-11-25 10:17:05] local.DEBUG: Cache warmed: 214 keys",
];

/// Fills `model` for `demo`: the Logs demos' services, two Laravel sites,
/// and `shop`'s log followed.
pub(super) fn fill(model: &mut Model, demo: Demo) {
    model.update(Update::Services(vec![
        service("caddy", ServiceState::Running),
        service("php-fpm-8.4", ServiceState::Running),
    ]));
    let mut list = model.session.sites.clone();
    for site in &mut list.sites {
        if !matches!(site.name.as_str(), "shop" | "billing-api") {
            site.kind = SiteKind::Php;
        }
    }
    model.update(Update::Sites(list));
    // As the screen does when it opens: the model asks the bridge to follow.
    let shop = Route::Logs(LogSource::Site("shop".into()));
    model.apply_action(Action::Navigate(shop));
    let lines = LINES
        .iter()
        .zip(0..)
        .map(|(text, seq)| SiteLogLine {
            seq,
            text: (*text).into(),
        })
        .collect();
    model.update(Update::SiteLog {
        site: "shop".into(),
        lines,
    });
    if demo == Demo::LogsLaravelErrors {
        model.apply_action(Action::ToggleLogLevel(LogLevel::Info));
        model.apply_action(Action::ToggleLogLevel(LogLevel::Warning));
    }
}
