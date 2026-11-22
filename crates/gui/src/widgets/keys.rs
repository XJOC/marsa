//! The keyboard of a group of buttons that is one Tab stop (the WAI-ARIA
//! "roving focus"): the tabs, the segmented control, and the version picker
//! all read the same keys the same way.

use eframe::egui::{EventFilter, Key, Response, Ui};

use crate::theme::Direction;

/// A move of the keyboard focus inside a group of buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Previous,
    Next,
    First,
    Last,
}

/// The stop the focus moves to: it stops at both ends, it does not wrap.
/// `stops` is how many buttons the group has.
pub fn moved(from: usize, stops: usize, step: Step) -> usize {
    let last = stops.saturating_sub(1);
    match step {
        Step::Previous => from.min(last).saturating_sub(1),
        Step::Next => from.saturating_add(1).min(last),
        Step::First => 0,
        Step::Last => last,
    }
}

/// What a key does in the group: the arrows along the row, and up and down,
/// move the focus; Home and End go to the ends. "Forward" along the row is
/// to the right in a left-to-right language.
pub fn step_for(key: Key, direction: Direction) -> Option<Step> {
    let along = |forward: bool| {
        if forward != direction.is_rtl() {
            Step::Next
        } else {
            Step::Previous
        }
    };
    match key {
        Key::ArrowRight => Some(along(true)),
        Key::ArrowLeft => Some(along(false)),
        Key::ArrowDown => Some(Step::Next),
        Key::ArrowUp => Some(Step::Previous),
        Key::Home => Some(Step::First),
        Key::End => Some(Step::Last),
        _ => None,
    }
}

/// The keys the group reads, in the order the first match wins. Enter and
/// Space are not here: egui turns them into a click on the focused button.
const KEYS: [Key; 6] = [
    Key::ArrowRight,
    Key::ArrowLeft,
    Key::ArrowDown,
    Key::ArrowUp,
    Key::Home,
    Key::End,
];

/// Moves the keyboard focus inside a group by the key pressed this frame.
/// The focused button asks egui to leave the arrows to the group. Returns
/// the index of the button that now has the focus, when a key moved it (the
/// same index at an end). The caller decides what moving means: a version
/// only gets the focus, a tab or an option is also chosen.
pub fn roving_keys(ui: &Ui, direction: Direction, responses: &[Response]) -> Option<usize> {
    let at = responses.iter().position(Response::has_focus)?;
    ui.memory_mut(|memory| {
        memory.set_focus_lock_filter(
            responses[at].id,
            EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                ..EventFilter::default()
            },
        );
    });
    let step = ui.input(|input| {
        KEYS.into_iter()
            .find(|key| input.key_pressed(*key))
            .and_then(|key| step_for(key, direction))
    })?;
    let target = moved(at, responses.len(), step);
    responses[target].request_focus();
    Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_moves_one_stop_and_stops_at_both_ends() {
        assert_eq!(moved(0, 4, Step::Next), 1);
        assert_eq!(moved(2, 4, Step::Next), 3);
        assert_eq!(moved(3, 4, Step::Next), 3, "no wrap at the end");
        assert_eq!(moved(1, 4, Step::Previous), 0);
        assert_eq!(moved(0, 4, Step::Previous), 0, "no wrap at the start");
        assert_eq!(moved(2, 4, Step::First), 0);
        assert_eq!(moved(1, 4, Step::Last), 3);
        // An empty group and a stale position never go out of range.
        assert_eq!(moved(0, 0, Step::Next), 0);
        assert_eq!(moved(0, 0, Step::Last), 0);
        assert_eq!(moved(9, 4, Step::Previous), 2);
        assert_eq!(moved(usize::MAX, 4, Step::Next), 3);
    }

    #[test]
    fn the_keys_map_to_steps_and_a_right_to_left_row_swaps_the_arrows() {
        let ltr = |key| step_for(key, Direction::Ltr);
        let rtl = |key| step_for(key, Direction::Rtl);
        assert_eq!(ltr(Key::ArrowRight), Some(Step::Next));
        assert_eq!(ltr(Key::ArrowLeft), Some(Step::Previous));
        assert_eq!(rtl(Key::ArrowRight), Some(Step::Previous));
        assert_eq!(rtl(Key::ArrowLeft), Some(Step::Next));
        for direction in [Direction::Ltr, Direction::Rtl] {
            assert_eq!(step_for(Key::ArrowDown, direction), Some(Step::Next));
            assert_eq!(step_for(Key::ArrowUp, direction), Some(Step::Previous));
            assert_eq!(step_for(Key::Home, direction), Some(Step::First));
            assert_eq!(step_for(Key::End, direction), Some(Step::Last));
            // Enter and Space are clicks, and Tab is egui's.
            for key in [Key::Enter, Key::Space, Key::Tab, Key::A] {
                assert_eq!(step_for(key, direction), None);
            }
        }
        assert!(
            KEYS.iter()
                .all(|key| step_for(*key, Direction::Ltr).is_some())
        );
    }
}
