//! The demo of the Workers card on a Laravel site's page, in its four
//! states: `MARSA_GUI_ROUTE=site:shop` has the queue worker running and the
//! scheduler off; `site:billing-api` has the queue worker failed (with a log
//! to open) and the scheduler starting.

use marsa_core::types::{LogLine, LogStream, ServiceState};

use super::{id, service};
use crate::model::{Model, Update};

/// The lines the failed worker wrote before it stopped.
const FAILED_LOG: [&str; 3] = [
    "   INFO  Processing jobs from the [default] queue.",
    "  2026-11-26 09:14:02 App\\Jobs\\SendInvoice ........................ RUNNING",
    "   Illuminate\\Database\\QueryException  SQLSTATE[HY000] [2002] Connection refused",
];

/// Fills `model`: the two sites' settings, and the workers the daemon lists.
pub(super) fn fill(model: &mut Model) {
    let mut list = model.session.sites.clone();
    for site in &mut list.sites {
        match site.name.as_str() {
            "shop" => site.queue = true,
            "billing-api" => {
                site.queue = true;
                site.schedule = true;
            }
            _ => {}
        }
    }
    model.update(Update::Sites(list));
    let mut running = service("queue-shop", ServiceState::Running);
    running.pid = Some(4310);
    let mut failed = service(
        "queue-billing-api",
        ServiceState::Failed {
            code: "worker_failed".into(),
        },
    );
    failed.restarts = 8;
    // The scheduler of `billing-api` is on and not listed yet: starting.
    model.update(Update::Services(vec![
        service("caddy", ServiceState::Running),
        service("php-fpm-8.4", ServiceState::Running),
        running,
        failed,
    ]));
    let lines = FAILED_LOG.iter().zip(0..).map(|(text, seq)| LogLine {
        service: id("queue-billing-api"),
        seq,
        at_ms: 0,
        stream: LogStream::Stdout,
        text: (*text).into(),
    });
    model.update(Update::Log(lines.collect()));
}
