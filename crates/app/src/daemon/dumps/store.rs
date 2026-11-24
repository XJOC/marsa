//! The dumps in memory: a ring that drops the oldest first.

use std::collections::VecDeque;

use marsa_core::types::Dump;

/// The most dumps kept.
pub(super) const MAX_DUMPS: usize = 500;
/// The most text kept, over all dumps.
pub(super) const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
pub(super) struct Store {
    /// Oldest first.
    dumps: VecDeque<Dump>,
    /// The text and strings of `dumps`, by [`cost`].
    bytes: usize,
    /// The `id` of the newest dump ever kept. Never goes back, so an id is
    /// not used twice, also after [`Store::clear`].
    latest_id: u64,
}

impl Store {
    /// Keeps `dump` as the newest, with the next id and the time `now`
    /// (milliseconds since the Unix epoch), then drops the oldest ones until
    /// the limits hold. Returns the id.
    pub(super) fn push(&mut self, mut dump: Dump, now: u64) -> u64 {
        self.latest_id += 1;
        dump.id = self.latest_id;
        // The daemon's clock: the line's own is not to be trusted.
        dump.received_at = now;
        self.bytes += cost(&dump);
        self.dumps.push_back(dump);
        while self.dumps.len() > MAX_DUMPS || self.bytes > MAX_BYTES {
            let Some(oldest) = self.dumps.pop_front() else {
                break;
            };
            self.bytes -= cost(&oldest);
        }
        self.latest_id
    }

    /// The dumps with an id over `after` (all, if `None`), newest first.
    pub(super) fn list(&self, after: Option<u64>) -> Vec<Dump> {
        let after = after.unwrap_or(0);
        self.dumps
            .iter()
            .rev()
            .take_while(|dump| dump.id > after)
            .cloned()
            .collect()
    }

    pub(super) fn clear(&mut self) {
        self.dumps.clear();
        self.bytes = 0;
    }

    pub(super) fn latest_id(&self) -> u64 {
        self.latest_id
    }

    pub(super) fn count(&self) -> usize {
        self.dumps.len()
    }
}

/// What one dump takes in memory, near enough: its strings and a fixed part.
fn cost(dump: &Dump) -> usize {
    let strings = [&dump.source_file, &dump.uri, &dump.method, &dump.label];
    let optional: usize = strings
        .iter()
        .map(|text| text.as_ref().map_or(0, String::len))
        .sum();
    dump.text.len() + optional + 128
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(text: &str) -> Dump {
        Dump {
            id: 0,
            received_at: 0,
            source_file: None,
            source_line: None,
            uri: None,
            method: None,
            label: None,
            text: text.into(),
            truncated: false,
        }
    }

    #[test]
    fn ids_grow_and_the_list_is_newest_first() {
        let mut store = Store::default();
        assert_eq!(store.push(dump("a"), 10), 1);
        assert_eq!(store.push(dump("b"), 20), 2);
        let all = store.list(None);
        assert_eq!(
            all.iter()
                .map(|d| (d.id, d.received_at))
                .collect::<Vec<_>>(),
            [(2, 20), (1, 10)]
        );
        let newer = store.list(Some(1));
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].text, "b");
        assert!(store.list(Some(2)).is_empty());
    }

    #[test]
    fn an_id_is_not_used_again_after_a_clear() {
        let mut store = Store::default();
        store.push(dump("a"), 0);
        store.clear();
        assert_eq!(store.count(), 0);
        assert_eq!(store.latest_id(), 1);
        assert_eq!(store.push(dump("b"), 0), 2);
    }

    #[test]
    fn the_ring_drops_the_oldest_at_500() {
        let mut store = Store::default();
        for i in 0..MAX_DUMPS + 20 {
            store.push(dump(&i.to_string()), 0);
        }
        assert_eq!(store.count(), MAX_DUMPS);
        let all = store.list(None);
        assert_eq!(all[0].id, (MAX_DUMPS + 20) as u64);
        assert_eq!(all.last().map(|d| d.id), Some(21));
    }

    #[test]
    fn the_ring_drops_the_oldest_at_16_mib_of_text() {
        let mut store = Store::default();
        let big = "x".repeat(200 * 1024);
        for _ in 0..100 {
            store.push(dump(&big), 0);
        }
        // 100 x 200 KiB is 20 MiB: the oldest went.
        assert!(store.count() < 100);
        assert!(store.count() * big.len() <= MAX_BYTES);
        assert_eq!(store.list(None)[0].id, 100);
    }
}
