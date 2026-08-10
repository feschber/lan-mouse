use std::collections::{HashMap, HashSet};

use input_event::scancode;

use crate::Position;

/// Tracks the keys held down while capture is inactive and reports
/// when the bind configured for a position becomes fully held.
///
/// Backends that can observe key events outside of an active capture
/// (a global keyboard hook or event tap) feed every such key into
/// [`Self::key_event`] and begin capture at the returned position,
/// exactly as they would for a pointer crossing the screen edge.
#[derive(Debug, Default)]
pub(crate) struct EnterBindTracker {
    binds: HashMap<Position, Vec<scancode::Linux>>,
    pressed: HashSet<scancode::Linux>,
}

impl EnterBindTracker {
    pub(crate) fn set_binds(&mut self, binds: HashMap<Position, Vec<scancode::Linux>>) {
        self.binds = binds;
        // A bind that is removed while its keys are held must not fire
        // once a *different* bind completes, so start from a clean slate.
        self.pressed.clear();
    }

    /// whether any bind is configured — lets a backend skip the
    /// bookkeeping entirely in the common case
    pub(crate) fn is_empty(&self) -> bool {
        self.binds.is_empty()
    }

    /// Forget every held key.
    ///
    /// Called once a bind has fired: its keys are still physically
    /// down, and without this the *next* key release would leave a
    /// subset of the bind held, letting a single further keypress
    /// re-trigger it.
    pub(crate) fn clear(&mut self) {
        self.pressed.clear();
    }

    /// Record a key event and report the position to enter, if the
    /// bind for one just completed.
    ///
    /// `active` is the set of positions that currently have a client;
    /// binds for any other position are ignored, so a bind can never
    /// enter a client an edge crossing could not.
    pub(crate) fn key_event(
        &mut self,
        key: scancode::Linux,
        pressed: bool,
        active: &HashSet<Position>,
    ) -> Option<Position> {
        if !pressed {
            self.pressed.remove(&key);
            // binds fire on the last key going *down*, never on release
            return None;
        }
        self.pressed.insert(key);
        self.binds
            .iter()
            .filter(|(pos, bind)| !bind.is_empty() && active.contains(pos))
            .find(|(_, bind)| bind.iter().all(|k| self.pressed.contains(k)))
            .map(|(&pos, _)| pos)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use input_event::scancode::Linux::{
        KeyA, KeyB, KeyLeftAlt, KeyLeftCtrl, KeyLeftShift, KeyRight, KeyUp,
    };

    fn tracker(binds: &[(Position, &[scancode::Linux])]) -> EnterBindTracker {
        let mut tracker = EnterBindTracker::default();
        tracker.set_binds(
            binds
                .iter()
                .map(|(pos, keys)| (*pos, keys.to_vec()))
                .collect(),
        );
        tracker
    }

    fn active(positions: &[Position]) -> HashSet<Position> {
        positions.iter().copied().collect()
    }

    #[test]
    fn fires_once_every_key_is_held() {
        let mut t = tracker(&[(Position::Right, &[KeyLeftCtrl, KeyLeftAlt, KeyRight])]);
        let active = active(&[Position::Right]);

        assert_eq!(t.key_event(KeyLeftCtrl, true, &active), None);
        assert_eq!(t.key_event(KeyLeftAlt, true, &active), None);
        assert_eq!(
            t.key_event(KeyRight, true, &active),
            Some(Position::Right),
            "bind completes on the last key"
        );
    }

    #[test]
    fn order_of_the_keys_does_not_matter() {
        let mut t = tracker(&[(Position::Top, &[KeyLeftCtrl, KeyUp])]);
        let active = active(&[Position::Top]);

        assert_eq!(t.key_event(KeyUp, true, &active), None);
        assert_eq!(t.key_event(KeyLeftCtrl, true, &active), Some(Position::Top));
    }

    #[test]
    fn does_not_fire_on_release() {
        let mut t = tracker(&[(Position::Top, &[KeyLeftCtrl, KeyUp])]);
        let active = active(&[Position::Top]);

        t.key_event(KeyLeftCtrl, true, &active);
        t.key_event(KeyUp, true, &active);
        assert_eq!(
            t.key_event(KeyUp, false, &active),
            None,
            "releasing a key must never enter a client"
        );
    }

    #[test]
    fn ignores_positions_without_a_client() {
        let mut t = tracker(&[(Position::Right, &[KeyLeftCtrl, KeyRight])]);
        let nothing_active = active(&[]);

        t.key_event(KeyLeftCtrl, true, &nothing_active);
        assert_eq!(
            t.key_event(KeyRight, true, &nothing_active),
            None,
            "a bind must not enter a position that has no client"
        );
    }

    #[test]
    fn a_superset_of_the_bind_still_fires() {
        // holding an extra key must not block the bind: users hold
        // modifiers in whatever order and combination they like
        let mut t = tracker(&[(Position::Left, &[KeyLeftCtrl])]);
        let active = active(&[Position::Left]);

        t.key_event(KeyLeftShift, true, &active);
        assert_eq!(
            t.key_event(KeyLeftCtrl, true, &active),
            Some(Position::Left)
        );
    }

    #[test]
    fn unrelated_keys_do_not_fire() {
        let mut t = tracker(&[(Position::Left, &[KeyLeftCtrl, KeyA])]);
        let active = active(&[Position::Left]);

        assert_eq!(t.key_event(KeyB, true, &active), None);
        assert_eq!(t.key_event(KeyLeftCtrl, true, &active), None);
    }

    #[test]
    fn clear_prevents_a_held_bind_from_retriggering() {
        let mut t = tracker(&[(Position::Top, &[KeyLeftCtrl, KeyUp])]);
        let active = active(&[Position::Top]);

        t.key_event(KeyLeftCtrl, true, &active);
        assert_eq!(t.key_event(KeyUp, true, &active), Some(Position::Top));

        // capture has begun; the keys are still physically down
        t.clear();
        assert_eq!(
            t.key_event(KeyUp, true, &active),
            None,
            "the whole bind has to be pressed again"
        );
        t.key_event(KeyLeftCtrl, true, &active);
        assert_eq!(
            t.key_event(KeyUp, false, &active),
            None,
            "and it still may not fire on release"
        );
    }

    #[test]
    fn keys_released_out_of_sight_do_not_stay_held() {
        // Capture can also begin by crossing an edge, with part of a
        // bind already held. Key events then go to the remote client
        // instead of here, so the release of those keys is never
        // seen — the backend clears for exactly this reason.
        let mut t = tracker(&[(Position::Top, &[KeyLeftCtrl, KeyUp])]);
        let active = active(&[Position::Top]);

        t.key_event(KeyLeftCtrl, true, &active);
        t.clear(); // capture began; ctrl is released unobserved

        assert_eq!(
            t.key_event(KeyUp, true, &active),
            None,
            "ctrl is not actually held anymore"
        );
    }

    #[test]
    fn empty_bind_never_fires() {
        let mut t = tracker(&[(Position::Left, &[])]);
        let active = active(&[Position::Left]);
        assert_eq!(t.key_event(KeyA, true, &active), None);
    }

    #[test]
    fn setting_binds_forgets_held_keys() {
        let mut t = tracker(&[(Position::Left, &[KeyLeftCtrl, KeyA])]);
        let active = active(&[Position::Left, Position::Right]);
        t.key_event(KeyLeftCtrl, true, &active);

        // reconfigured while ctrl is still held
        t.set_binds(HashMap::from([(
            Position::Right,
            vec![KeyLeftCtrl, KeyRight],
        )]));
        assert_eq!(
            t.key_event(KeyRight, true, &active),
            None,
            "ctrl was held before the new bind existed"
        );
    }
}
