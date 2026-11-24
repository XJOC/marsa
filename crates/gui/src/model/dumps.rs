//! The dumps `marsad` caught from PHP sites, as the window keeps them: the
//! newest first, which one is shown, and how many came since the user last
//! looked. The bridge reads the daemon (at the connection, when the screen
//! opens, and at each `dumps_changed` event) and sends only what is new; the
//! model merges it and keeps no more than the daemon does.

use marsa_core::types::{Dump, DumpList, DumpsStatus, MAX_KEPT_BYTES, MAX_KEPT_DUMPS};

use super::*;

/// What the window knows of the dumps. Part of the [`Session`]: a new
/// connection reads it again.
#[derive(Debug, Default)]
pub struct DumpsState {
    /// Newest first, at most [`MAX_KEPT_DUMPS`] and [`MAX_KEPT_BYTES`] of
    /// text, as the daemon keeps them.
    pub dumps: Vec<Dump>,
    /// What the dump server is doing, as last read.
    pub status: DumpsStatus,
    /// The id of the newest dump the daemon ever kept.
    pub latest_id: u64,
    /// Whether the daemon has answered once.
    pub loaded: bool,
    /// The dump the user chose. Nothing chosen: the newest one is shown.
    pub selected: Option<u64>,
    /// The Dumps screen is open: what arrives is seen as it arrives.
    open: bool,
    /// The newest id the user has seen: every dump over it is new.
    seen: u64,
    /// The weight of `dumps` ([`Dump::weight`]).
    bytes: usize,
    /// Why the last read failed: a client error code.
    pub error: Option<String>,
}

impl DumpsState {
    /// The dump the detail shows: the chosen one, else the newest.
    pub fn shown(&self) -> Option<&Dump> {
        match self.selected {
            Some(id) => self.dumps.iter().find(|dump| dump.id == id),
            None => self.dumps.first(),
        }
        .or_else(|| self.dumps.first())
    }

    /// Whether `dump` came after the user last looked.
    pub fn is_new(&self, dump: &Dump) -> bool {
        !self.open && dump.id > self.seen
    }

    /// How many dumps came after the user last looked, for the sidebar.
    /// None while the screen is open.
    pub fn unseen(&self) -> u64 {
        if self.open {
            return 0;
        }
        self.dumps
            .iter()
            .take_while(|dump| dump.id > self.seen)
            .count() as u64
    }

    /// Dumps go to the window while the dump server runs and its port is
    /// the daemon's, or it is on and the port is taken.
    pub fn capture_on(&self) -> bool {
        self.status != DumpsStatus::Disabled
    }

    /// The Dumps screen opened or closed. Opening counts every dump as
    /// seen, and the ones that come while it is open too.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
        self.seen = self.latest_id;
    }

    /// Keeps `dumps` within what the daemon keeps: no more than it says it
    /// holds (it cleared, or dropped the oldest), its count, and its bytes.
    fn trim(&mut self, count: u64) {
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        self.bytes = self.dumps.iter().map(Dump::weight).sum();
        while self.dumps.len() > count.min(MAX_KEPT_DUMPS) || self.bytes > MAX_KEPT_BYTES {
            let Some(oldest) = self.dumps.pop() else {
                break;
            };
            self.bytes -= oldest.weight();
        }
    }
}

impl Model {
    /// What the bridge read: the dumps newer than the last it sent (all of
    /// them when `replace`), with the daemon's status and count.
    pub(super) fn set_dumps(&mut self, list: DumpList, replace: bool) {
        let dumps = &mut self.session.dumps;
        dumps.error = None;
        dumps.loaded = true;
        dumps.status = list.status;
        dumps.latest_id = dumps.latest_id.max(list.latest_id);
        if replace {
            dumps.dumps = list.dumps;
        } else {
            // Only the ones the window does not have: a read that raced
            // with another sends some twice.
            let newest = dumps.dumps.first().map_or(0, |dump| dump.id);
            let mut fresh: Vec<Dump> = list
                .dumps
                .into_iter()
                .filter(|dump| dump.id > newest)
                .collect();
            fresh.append(&mut dumps.dumps);
            dumps.dumps = fresh;
        }
        dumps.trim(list.count);
        if let Some(id) = dumps.selected
            && !dumps.dumps.iter().any(|dump| dump.id == id)
        {
            dumps.selected = None;
        }
        // Seen as they arrive, while the screen is open.
        if dumps.open {
            dumps.seen = dumps.latest_id;
        }
    }

    /// A row of the list was chosen.
    pub(super) fn choose_dump(&mut self, id: u64) {
        if self.session.dumps.dumps.iter().any(|dump| dump.id == id) {
            self.session.dumps.selected = Some(id);
        }
    }

    /// "Clear": the daemon forgets every dump.
    pub(super) fn clear_dumps(&self) -> Option<Command> {
        let any = !self.session.dumps.dumps.is_empty();
        (self.is_connected() && any).then_some(Command::ClearDumps)
    }

    /// The switch "Capture": on or off, the other way than now.
    pub(super) fn toggle_capture(&self) -> Option<Command> {
        self.is_connected()
            .then(|| Command::SetDumpsCapture(!self.session.dumps.capture_on()))
    }

    /// A read of the dumps failed. The screen says why when it has nothing
    /// to show; a call (clear, the switch) that failed is a notice.
    pub(super) fn dumps_failed(&mut self, code: String) {
        self.session.dumps.error = Some(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::connected;

    fn dump(id: u64) -> Dump {
        Dump {
            id,
            received_at: 1_800_000_000_000 + id,
            source_file: Some("/app/routes/web.php".into()),
            source_line: Some(7),
            uri: None,
            method: None,
            label: None,
            text: format!("{id}"),
            truncated: false,
        }
    }

    /// The ids newest first, as the daemon answers.
    fn list(ids: &[u64], count: u64) -> DumpList {
        DumpList {
            status: DumpsStatus::Listening,
            latest_id: ids.first().copied().unwrap_or(0),
            count,
            dumps: ids.iter().map(|&id| dump(id)).collect(),
        }
    }

    fn ids(model: &Model) -> Vec<u64> {
        model.session.dumps.dumps.iter().map(|d| d.id).collect()
    }

    fn sent(effect: Effect) -> Option<Command> {
        effect.command()
    }

    #[test]
    fn what_arrives_while_the_screen_is_shut_is_new_and_opening_clears_the_count() {
        let mut model = connected();
        model.update(Update::Dumps {
            list: list(&[2, 1], 2),
            replace: true,
        });
        assert_eq!(model.session.dumps.unseen(), 2);
        model.session.dumps.set_open(true);
        assert_eq!(model.session.dumps.unseen(), 0);
        // Seen as they arrive, while the screen is open.
        model.update(Update::Dumps {
            list: list(&[4, 3], 4),
            replace: false,
        });
        assert_eq!(ids(&model), [4, 3, 2, 1]);
        assert_eq!(model.session.dumps.unseen(), 0);
        // Shut: the next ones count, and the ones seen do not.
        model.session.dumps.set_open(false);
        model.update(Update::Dumps {
            list: list(&[5], 5),
            replace: false,
        });
        assert_eq!(model.session.dumps.unseen(), 1);
        assert!(model.session.dumps.is_new(&dump(5)));
        assert!(!model.session.dumps.is_new(&dump(4)));
    }

    #[test]
    fn a_dump_sent_twice_is_kept_once_and_the_after_read_keeps_the_order() {
        let mut model = connected();
        model.update(Update::Dumps {
            list: list(&[3, 2, 1], 3),
            replace: true,
        });
        // A racing read sends 3 again with the new 4.
        model.update(Update::Dumps {
            list: list(&[4, 3], 4),
            replace: false,
        });
        assert_eq!(ids(&model), [4, 3, 2, 1]);
        assert_eq!(model.session.dumps.latest_id, 4);
    }

    #[test]
    fn the_daemons_count_trims_what_it_cleared_or_dropped() {
        let mut model = connected();
        model.update(Update::Dumps {
            list: list(&[3, 2, 1], 3),
            replace: true,
        });
        // The CLI cleared, then two came: the daemon holds 2, the window
        // would have 5.
        model.update(Update::Dumps {
            list: list(&[5, 4], 2),
            replace: false,
        });
        assert_eq!(ids(&model), [5, 4]);
        // Cleared and nothing new.
        model.update(Update::Dumps {
            list: DumpList {
                latest_id: 5,
                count: 0,
                ..DumpList::default()
            },
            replace: false,
        });
        assert!(model.session.dumps.dumps.is_empty());
        assert_eq!(model.session.dumps.latest_id, 5);
    }

    #[test]
    fn the_window_keeps_no_more_than_the_daemon() {
        let mut model = connected();
        let many: Vec<u64> = (1..=600).rev().collect();
        model.update(Update::Dumps {
            list: list(&many, 600),
            replace: true,
        });
        assert_eq!(model.session.dumps.dumps.len(), MAX_KEPT_DUMPS);
        assert_eq!(model.session.dumps.dumps[0].id, 600);
        // By text too: 100 dumps of 200 KiB are 20 MiB.
        let mut big = list(&[], 100);
        big.dumps = (1..=100)
            .rev()
            .map(|id| Dump {
                text: "x".repeat(200 * 1024),
                ..dump(id)
            })
            .collect();
        let mut model = connected();
        model.update(Update::Dumps {
            list: big,
            replace: true,
        });
        let kept = &model.session.dumps.dumps;
        assert!(kept.len() < 100);
        assert!(kept.iter().map(Dump::weight).sum::<usize>() <= MAX_KEPT_BYTES);
        assert_eq!(kept[0].id, 100, "the newest stay");
    }

    #[test]
    fn the_newest_is_shown_until_one_is_chosen_and_a_gone_one_falls_back() {
        let mut model = connected();
        model.update(Update::Dumps {
            list: list(&[3, 2, 1], 3),
            replace: true,
        });
        assert_eq!(model.session.dumps.shown().map(|d| d.id), Some(3));
        model.apply_action(Action::ChooseDump(2));
        assert_eq!(model.session.dumps.shown().map(|d| d.id), Some(2));
        // A new dump does not move the choice.
        model.update(Update::Dumps {
            list: list(&[4], 4),
            replace: false,
        });
        assert_eq!(model.session.dumps.shown().map(|d| d.id), Some(2));
        // An id that is not in the list is not chosen.
        model.apply_action(Action::ChooseDump(99));
        assert_eq!(model.session.dumps.selected, Some(2));
        // Cleared elsewhere: back to the newest.
        model.update(Update::Dumps {
            list: list(&[5], 1),
            replace: false,
        });
        assert_eq!(model.session.dumps.selected, None);
        assert_eq!(model.session.dumps.shown().map(|d| d.id), Some(5));
    }

    #[test]
    fn clear_and_the_switch_are_commands_and_offline_they_are_not() {
        let mut model = connected();
        assert!(model.apply_action(Action::ClearDumps).is_none(), "no dumps");
        model.update(Update::Dumps {
            list: list(&[1], 1),
            replace: true,
        });
        let clear = sent(model.apply_action(Action::ClearDumps));
        assert!(matches!(clear, Some(Command::ClearDumps)), "{clear:?}");
        // On now, so the switch turns it off; off, on.
        let off = sent(model.apply_action(Action::ToggleDumps));
        assert!(
            matches!(off, Some(Command::SetDumpsCapture(false))),
            "{off:?}"
        );
        model.update(Update::Dumps {
            list: DumpList {
                status: DumpsStatus::Disabled,
                ..DumpList::default()
            },
            replace: false,
        });
        let on = sent(model.apply_action(Action::ToggleDumps));
        assert!(matches!(on, Some(Command::SetDumpsCapture(true))), "{on:?}");
        // A taken port is still "on": the switch turns it off.
        model.update(Update::Dumps {
            list: DumpList {
                status: DumpsStatus::PortInUse,
                ..DumpList::default()
            },
            replace: false,
        });
        assert!(model.session.dumps.capture_on());
        let mut offline = Model::default();
        assert!(offline.apply_action(Action::ToggleDumps).is_none());
        assert!(offline.apply_action(Action::ClearDumps).is_none());
    }

    #[test]
    fn a_failed_read_is_kept_and_the_next_answer_clears_it() {
        let mut model = connected();
        model.update(Update::DumpsFailed("not_found".into()));
        assert_eq!(model.session.dumps.error.as_deref(), Some("not_found"));
        assert!(!model.session.dumps.loaded);
        model.update(Update::Dumps {
            list: list(&[1], 1),
            replace: true,
        });
        assert!(model.session.dumps.error.is_none());
        assert!(model.session.dumps.loaded);
    }

    #[test]
    fn the_dumps_go_with_the_connection() {
        let mut model = connected();
        model.update(Update::Dumps {
            list: list(&[1], 1),
            replace: true,
        });
        model.update(Update::Reconnecting);
        assert!(model.session.dumps.dumps.is_empty());
        assert!(!model.session.dumps.loaded);
    }
}
