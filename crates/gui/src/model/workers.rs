//! A Laravel site's workers, as its page shows them: the switch of each, and
//! what the daemon says of the worker (`queue-<site>`, `schedule-<site>`).
//!
//! The setting is the daemon's: a switch sends the change, and keeps showing
//! the daemon's value, disabled, until the site list has the new one.
//! [Laravel: Livewire waiting for the refresh after `wire:click`, instead of
//! changing its own state first.]

use marsa_core::types::{ServiceId, ServiceStatus, SiteKey, WorkerKind};

use super::{Model, NoticeSource, SitesNotice};
use crate::bridge::Command;

impl Model {
    /// The status of worker `kind` of `site`, if the daemon lists it.
    pub fn worker_status(&self, kind: WorkerKind, site: SiteKey) -> Option<&ServiceStatus> {
        let id = ServiceId::Worker(kind, site);
        self.session.services.iter().find(|status| status.id == id)
    }

    /// Whether the change of the switch of worker `kind` of `site` is on its
    /// way to the daemon.
    pub fn worker_asked(&self, kind: WorkerKind, site: SiteKey) -> bool {
        let id = ServiceId::Worker(kind, site);
        self.session.workers_asked.contains_key(&id)
    }

    /// The switch of worker `kind` of `site`: the command that asks the
    /// daemon for `on`. Nothing while the last change waits, while the site
    /// list already has `on`, or with no daemon.
    pub(super) fn set_worker(
        &mut self,
        kind: WorkerKind,
        site: SiteKey,
        on: bool,
    ) -> Option<Command> {
        if !self.is_connected() || self.worker_asked(kind, site) {
            return None;
        }
        let sites = &self.session.sites.sites;
        let found = sites.iter().find(|found| found.name == site.as_str())?;
        if found.wants(kind) == on {
            return None;
        }
        // The notice was about the last action.
        self.session.notices.clear(NoticeSource::Sites);
        let id = ServiceId::Worker(kind, site);
        self.session.workers_asked.insert(id, on);
        Some(Command::SetWorker { kind, site, on })
    }

    /// The daemon refused the change, or could not be reached: the switch is
    /// free again, and the notice says why.
    pub(super) fn worker_not_saved(&mut self, kind: WorkerKind, site: SiteKey, code: String) {
        let id = ServiceId::Worker(kind, site);
        self.session.workers_asked.remove(&id);
        self.session.notices.set_sites(SitesNotice::Failed(code));
    }

    /// A new site list: each change it has is done, and a site that is gone
    /// takes its change with it.
    pub(super) fn settle_workers(&mut self) {
        let sites = &self.session.sites.sites;
        self.session.workers_asked.retain(|id, on| {
            let ServiceId::Worker(kind, site) = id else {
                return false;
            };
            let found = sites.iter().find(|found| found.name == site.as_str());
            found.is_some_and(|found| found.wants(*kind) != *on)
        });
    }
}

#[cfg(test)]
mod tests {
    use marsa_core::types::{Event, ServiceState, Site, SiteList};

    use super::*;
    use crate::fixtures::{connected, site};
    use crate::model::{Action, Update, apply};

    fn key(name: &str) -> SiteKey {
        SiteKey::new(name).unwrap()
    }

    fn with_sites(model: &mut Model, sites: Vec<Site>) {
        apply(
            model,
            Event::SitesChanged(SiteList {
                sites,
                ..SiteList::default()
            }),
        );
    }

    /// A connected model with the Laravel site `shop`, its queue worker as
    /// `queue` says.
    fn model(queue: bool) -> Model {
        let mut model = connected();
        let shop = Site {
            queue,
            ..site("shop", Some("8.4.27"), None)
        };
        with_sites(&mut model, vec![shop]);
        model
    }

    fn switch(model: &mut Model, kind: WorkerKind, on: bool) -> Option<Command> {
        let action = Action::SetWorker {
            kind,
            site: key("shop"),
            on,
        };
        model.apply_action(action).command()
    }

    fn queue_on(model: &Model) -> bool {
        model.session.sites.sites[0].queue
    }

    #[test]
    fn a_switch_sends_the_change_and_waits_for_the_site_list() {
        let mut model = model(false);
        let sent = switch(&mut model, WorkerKind::Queue, true);
        assert!(
            matches!(sent, Some(Command::SetWorker { kind: WorkerKind::Queue, site, on: true }) if site == key("shop")),
            "{sent:?}"
        );
        // The setting stays the daemon's, and the switch waits.
        assert!(!queue_on(&model));
        assert!(model.worker_asked(WorkerKind::Queue, key("shop")));
        assert!(!model.worker_asked(WorkerKind::Schedule, key("shop")));
        // A second press while it waits sends nothing.
        assert!(switch(&mut model, WorkerKind::Queue, true).is_none());
        // A list that does not have it yet (another change) keeps it waiting.
        with_sites(&mut model, vec![site("shop", Some("8.4.27"), None)]);
        assert!(model.worker_asked(WorkerKind::Queue, key("shop")));
        // The list with the new setting ends the wait.
        let shop = Site {
            queue: true,
            ..site("shop", Some("8.4.27"), None)
        };
        with_sites(&mut model, vec![shop]);
        assert!(queue_on(&model));
        assert!(!model.worker_asked(WorkerKind::Queue, key("shop")));
        // Off goes the same way.
        let off = switch(&mut model, WorkerKind::Queue, false);
        assert!(matches!(off, Some(Command::SetWorker { on: false, .. })));
    }

    #[test]
    fn a_refusal_frees_the_switch_and_says_why() {
        let mut model = model(false);
        assert!(switch(&mut model, WorkerKind::Schedule, true).is_some());
        model.update(Update::WorkerNotSaved {
            kind: WorkerKind::Schedule,
            site: key("shop"),
            code: "site_not_laravel".into(),
        });
        assert!(!model.worker_asked(WorkerKind::Schedule, key("shop")));
        assert_eq!(
            model.session.notices.sites(),
            Some(&SitesNotice::Failed("site_not_laravel".into()))
        );
        // The next press clears the notice and sends again.
        assert!(switch(&mut model, WorkerKind::Schedule, true).is_some());
        assert_eq!(model.session.notices.sites(), None);
    }

    #[test]
    fn nothing_is_sent_for_the_setting_in_force_a_missing_site_or_no_daemon() {
        let mut model = model(true);
        assert!(switch(&mut model, WorkerKind::Queue, true).is_none());
        let mut model = model_without_shop();
        assert!(switch(&mut model, WorkerKind::Queue, true).is_none());
        let mut offline = Model::default();
        assert!(switch(&mut offline, WorkerKind::Queue, true).is_none());
        assert!(offline.session.workers_asked.is_empty());
    }

    fn model_without_shop() -> Model {
        let mut model = connected();
        with_sites(&mut model, vec![site("blog", Some("8.4.27"), None)]);
        model
    }

    #[test]
    fn a_site_that_goes_away_or_a_lost_daemon_takes_its_change_with_it() {
        let mut model = model(false);
        assert!(switch(&mut model, WorkerKind::Queue, true).is_some());
        with_sites(&mut model, Vec::new());
        assert!(!model.worker_asked(WorkerKind::Queue, key("shop")));

        let mut model = self::model(false);
        assert!(switch(&mut model, WorkerKind::Queue, true).is_some());
        model.update(Update::Reconnecting);
        assert!(model.session.workers_asked.is_empty());
    }

    #[test]
    fn the_status_is_the_daemons_service_of_the_worker() {
        let mut model = model(true);
        assert_eq!(model.worker_status(WorkerKind::Queue, key("shop")), None);
        let status = ServiceStatus {
            id: ServiceId::Worker(WorkerKind::Queue, key("shop")),
            state: ServiceState::Running,
            pid: Some(42),
            restarts: 0,
            since_ms: None,
        };
        apply(&mut model, Event::ServiceChanged(status.clone()));
        assert_eq!(
            model.worker_status(WorkerKind::Queue, key("shop")),
            Some(&status)
        );
        assert_eq!(model.worker_status(WorkerKind::Schedule, key("shop")), None);
    }
}
