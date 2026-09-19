use std::collections::{HashMap, HashSet};

use input_event::{Event, KeyboardEvent, PointerEvent, scancode};
use serde::{Deserialize, Serialize};

/// X11-style modifier mask bits, as carried by
/// [`KeyboardEvent::Modifiers`].
mod mask {
    pub(super) const SHIFT: u32 = 1 << 0;
    pub(super) const LOCK: u32 = 1 << 1;
    pub(super) const CONTROL: u32 = 1 << 2;
    pub(super) const MOD1: u32 = 1 << 3; // Alt
    pub(super) const MOD4: u32 = 1 << 6; // Super / Command
}

/// The modifier bit a key contributes to the mask, if any.
fn modifier_bit(key: scancode::Linux) -> Option<u32> {
    use scancode::Linux::*;
    Some(match key {
        KeyLeftShift | KeyRightShift => mask::SHIFT,
        KeyCapsLock => mask::LOCK,
        KeyLeftCtrl | KeyRightCtrl => mask::CONTROL,
        KeyLeftAlt | KeyRightalt => mask::MOD1,
        KeyLeftMeta | KeyRightmeta => mask::MOD4,
        _ => return None,
    })
}

fn key_event(time: u32, key: scancode::Linux, state: u8) -> Event {
    Event::Keyboard(KeyboardEvent::Key {
        time,
        key: key as u32,
        state,
    })
}

/// A chord-specific override: while `modifier` is held, pressing
/// `trigger` sends `modifier` as `to` instead of its plain
/// [`KeyRemap`] mapping, for the rest of that hold. The motivating
/// case is `Cmd+Tab` on a Mac reaching a Windows peer as `Alt+Tab`
/// (app switcher) rather than `Ctrl+Tab` (which a plain Command→Control
/// mapping would otherwise produce, since `remap_keys` can't tell
/// `Cmd` alone from `Cmd+Tab`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChordRemap {
    pub modifier: scancode::Linux,
    pub trigger: scancode::Linux,
    pub to: scancode::Linux,
}

/// Resolution state of a chord-eligible modifier that's currently held.
#[derive(Debug, Clone, Copy)]
struct ChordEntry {
    trigger: scancode::Linux,
    to: scancode::Linux,
}

/// Rewrites keys on their way to another device.
///
/// The typical use is reconciling modifier layouts between operating
/// systems — sending Command from a Mac as Control, and Control as
/// Super, so muscle memory keeps working on a Windows or Linux peer.
/// `remap_chords` layers a second, narrower rewrite on top: a modifier
/// can be sent as something *else* specifically when a given trigger
/// key is pressed while it's held (`Cmd+Tab` → `Alt+Tab`), without
/// disturbing what it sends as on its own (`Cmd+C` → `Ctrl+C`).
///
/// Remapping deliberately happens on the *sending* side, right before
/// events go on the wire: everything local (the release bind, enter
/// binds, the host's own shortcuts) keeps seeing the physical keys.
#[derive(Debug, Default, Clone)]
pub(crate) struct KeyRemap {
    keys: HashMap<scancode::Linux, scancode::Linux>,
    /// Bit rewrites derived from `keys`, as (from, to) pairs.
    ///
    /// A modifier press arrives twice — once as a key and once as a
    /// mask update — and the two have to stay consistent, or the peer
    /// ends up holding one modifier while its mask claims another.
    bits: Vec<(u32, u32)>,
    /// chord rules, keyed by the physical modifier they apply to. Only
    /// one rule per modifier is supported — a later duplicate in config
    /// silently wins, like any other `HashMap` collision.
    chords: HashMap<scancode::Linux, ChordEntry>,
    /// chord modifiers currently held whose fate isn't decided yet — no
    /// down event has been sent to the peer for these at all
    pending: HashSet<scancode::Linux>,
    /// chord modifiers currently held that resolved to an override,
    /// and what they're being sent as
    active: HashMap<scancode::Linux, scancode::Linux>,
    /// last raw `Modifiers` mask seen, used to emit a corrected
    /// snapshot once a pending chord resolves (the original event for
    /// that hold was suppressed, since its outcome wasn't known yet)
    raw_mask: (u32, u32, u32, u32),
}

impl KeyRemap {
    pub(crate) fn new(
        keys: HashMap<scancode::Linux, scancode::Linux>,
        chords: Vec<ChordRemap>,
    ) -> Self {
        let bits = keys
            .iter()
            .filter(|(from, to)| from != to)
            .filter_map(|(&from, &to)| Some((modifier_bit(from)?, modifier_bit(to)?)))
            .filter(|(from, to)| from != to)
            .collect();
        let chords = chords
            .into_iter()
            .map(|c| {
                (
                    c.modifier,
                    ChordEntry {
                        trigger: c.trigger,
                        to: c.to,
                    },
                )
            })
            .collect();
        Self {
            keys,
            bits,
            chords,
            pending: HashSet::new(),
            active: HashMap::new(),
            raw_mask: (0, 0, 0, 0),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.chords.is_empty()
    }

    /// the key `key` is sent as outside of any chord resolution
    fn plain_key(&self, key: scancode::Linux) -> scancode::Linux {
        self.keys.get(&key).copied().unwrap_or(key)
    }

    /// The key a currently-held physical key should be released as, or
    /// `None` if no matching down was ever actually sent (it was still
    /// `pending` on an unresolved chord) — synthesizing a key-up in
    /// that case would release a key the peer was never told was
    /// pressed. Clears any chord bookkeeping for `key` as a side
    /// effect, matching the one-shot "drain and release everything"
    /// use at [`crate::capture::CaptureTask::release_capture`].
    pub(crate) fn release_key(&mut self, key: scancode::Linux) -> Option<scancode::Linux> {
        if let Some(to) = self.active.remove(&key) {
            return Some(to);
        }
        if self.pending.remove(&key) {
            return None;
        }
        Some(self.plain_key(key))
    }

    fn remap_mask(&self, m: u32) -> u32 {
        if self.bits.is_empty() {
            return m;
        }
        // Clear every source bit before setting any target bit, so a
        // swap (A→B together with B→A) doesn't lose one of the two.
        let mut out = m;
        for (from, _) in &self.bits {
            out &= !from;
        }
        for (from, to) in &self.bits {
            if m & from != 0 {
                out |= to;
            }
        }
        out
    }

    /// [`Self::remap_mask`], further corrected for chord-eligible
    /// modifiers currently `pending` (bit omitted — no down sent yet)
    /// or `active` (bit swapped to the override's, not the plain
    /// mapping's).
    fn remap_mask_dynamic(&self, m: u32) -> u32 {
        let mut out = self.remap_mask(m);
        for &phys in self.chords.keys() {
            let Some(phys_bit) = modifier_bit(phys) else {
                continue;
            };
            if m & phys_bit == 0 {
                continue;
            }
            let static_target = self.plain_key(phys);
            if let Some(b) = modifier_bit(static_target) {
                out &= !b;
            }
            match self.active.get(&phys) {
                Some(&to) => {
                    if let Some(b) = modifier_bit(to) {
                        out |= b;
                    }
                }
                None if self.pending.contains(&phys) => { /* omit: no down sent yet */ }
                None => {
                    if let Some(b) = modifier_bit(static_target) {
                        out |= b;
                    }
                }
            }
        }
        out
    }

    /// a fresh `Modifiers` event from the last raw mask seen, reflecting
    /// the current chord resolution — sent whenever that resolution
    /// changes, since the original event (if any) for this state may
    /// have been suppressed while the outcome was still undecided
    fn remapped_modifiers_event(&self) -> Event {
        let (depressed, latched, locked, group) = self.raw_mask;
        Event::Keyboard(KeyboardEvent::Modifiers {
            depressed: self.remap_mask_dynamic(depressed),
            latched: self.remap_mask_dynamic(latched),
            locked: self.remap_mask_dynamic(locked),
            group,
        })
    }

    /// resolves every still-pending chord modifier as "plain" — its
    /// static mapping, same as if no chord rule applied this hold —
    /// and emits the Key + Modifiers events that were held back
    fn flush_pending(&mut self, time: u32) -> Vec<Event> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Event> = std::mem::take(&mut self.pending)
            .into_iter()
            .map(|k| key_event(time, self.plain_key(k), 1))
            .collect();
        out.push(self.remapped_modifiers_event());
        out
    }

    fn apply_key(&mut self, time: u32, k: scancode::Linux, state: u8) -> Vec<Event> {
        if state == 1 {
            if self.chords.contains_key(&k) {
                self.pending.insert(k);
                return Vec::new();
            }
            if let Some(&modifier) = self.pending.iter().find(|&&m| self.chords[&m].trigger == k) {
                self.pending.remove(&modifier);
                let to = self.chords[&modifier].to;
                self.active.insert(modifier, to);
                return vec![
                    key_event(time, to, 1),
                    self.remapped_modifiers_event(),
                    key_event(time, self.plain_key(k), 1),
                ];
            }
            if modifier_bit(k).is_some() {
                // an unrelated modifier (e.g. Shift for Cmd+Shift+Tab)
                // — pass through, leave any pending chord waiting
                return vec![key_event(time, self.plain_key(k), 1)];
            }
            // a regular key that isn't our trigger: the chord
            // opportunity is over
            let mut out = self.flush_pending(time);
            out.push(key_event(time, self.plain_key(k), 1));
            out
        } else {
            if let Some(to) = self.active.remove(&k) {
                return vec![key_event(time, to, 0)];
            }
            if self.pending.remove(&k) {
                // tapped alone and released before it resolved either way
                return vec![
                    key_event(time, self.plain_key(k), 1),
                    key_event(time, self.plain_key(k), 0),
                ];
            }
            if modifier_bit(k).is_some() {
                return vec![key_event(time, self.plain_key(k), 0)];
            }
            let mut out = self.flush_pending(time);
            out.push(key_event(time, self.plain_key(k), 0));
            out
        }
    }

    /// Rewrite an outgoing event. Anything that isn't a key, a
    /// modifier update, or a deliberate pointer action (button, scroll)
    /// passes through untouched; plain cursor motion never disturbs an
    /// in-progress chord decision.
    pub(crate) fn apply(&mut self, event: Event) -> Vec<Event> {
        if self.is_empty() {
            return vec![event];
        }
        match event {
            Event::Keyboard(KeyboardEvent::Key { time, key, state }) => {
                match scancode::Linux::try_from(key) {
                    Ok(k) => self.apply_key(time, k, state),
                    Err(_) => vec![Event::Keyboard(KeyboardEvent::Key { time, key, state })],
                }
            }
            Event::Keyboard(KeyboardEvent::Modifiers {
                depressed,
                latched,
                locked,
                group,
            }) => {
                self.raw_mask = (depressed, latched, locked, group);
                if self.pending.is_empty() {
                    vec![self.remapped_modifiers_event()]
                } else {
                    Vec::new()
                }
            }
            Event::Pointer(PointerEvent::Motion { .. }) => vec![event],
            e => {
                // a click or a scroll is a deliberate action, not a
                // chord in progress — resolve any pending chord first
                let mut out = self.flush_pending(0);
                out.push(e);
                out
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use input_event::scancode::Linux::{
        KeyA, KeyC, KeyLeftAlt, KeyLeftCtrl, KeyLeftMeta, KeyLeftShift, KeyTab,
    };

    /// the swap this exists for: Command ⇄ Control
    fn swap() -> KeyRemap {
        KeyRemap::new(
            HashMap::from([(KeyLeftMeta, KeyLeftCtrl), (KeyLeftCtrl, KeyLeftMeta)]),
            Vec::new(),
        )
    }

    /// Command→Control, plus Cmd+Tab → Alt+Tab
    fn swap_with_chord() -> KeyRemap {
        KeyRemap::new(
            HashMap::from([(KeyLeftMeta, KeyLeftCtrl), (KeyLeftCtrl, KeyLeftMeta)]),
            vec![ChordRemap {
                modifier: KeyLeftMeta,
                trigger: KeyTab,
                to: KeyLeftAlt,
            }],
        )
    }

    fn key_ev(key: scancode::Linux, state: u8) -> Event {
        Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key: key as u32,
            state,
        })
    }

    fn mods(depressed: u32) -> Event {
        Event::Keyboard(KeyboardEvent::Modifiers {
            depressed,
            latched: 0,
            locked: 0,
            group: 0,
        })
    }

    #[test]
    fn swaps_both_directions() {
        let mut r = swap();
        assert_eq!(
            r.apply(key_ev(KeyLeftMeta, 1)),
            vec![key_ev(KeyLeftCtrl, 1)]
        );
        assert_eq!(
            r.apply(key_ev(KeyLeftCtrl, 1)),
            vec![key_ev(KeyLeftMeta, 1)]
        );
    }

    #[test]
    fn leaves_unmapped_keys_alone() {
        let mut r = swap();
        assert_eq!(r.apply(key_ev(KeyA, 1)), vec![key_ev(KeyA, 1)]);
        assert_eq!(r.apply(key_ev(KeyLeftAlt, 1)), vec![key_ev(KeyLeftAlt, 1)]);
    }

    #[test]
    fn key_state_is_preserved() {
        let mut r = swap();
        assert_eq!(
            r.apply(key_ev(KeyLeftMeta, 0)),
            vec![key_ev(KeyLeftCtrl, 0)]
        );
    }

    #[test]
    fn modifier_mask_swaps_with_the_keys() {
        // the whole point: the mask must agree with the key events,
        // or the peer holds Control while its mask says Super
        let mut r = swap();
        assert_eq!(r.apply(mods(mask::MOD4)), vec![mods(mask::CONTROL)]);
        let mut r = swap();
        assert_eq!(r.apply(mods(mask::CONTROL)), vec![mods(mask::MOD4)]);
    }

    #[test]
    fn both_modifiers_held_survives_the_swap() {
        // a plain "clear then set" that forgot to clear first would
        // drop one of the two bits here
        let mut r = swap();
        assert_eq!(
            r.apply(mods(mask::CONTROL | mask::MOD4)),
            vec![mods(mask::CONTROL | mask::MOD4)]
        );
    }

    #[test]
    fn untouched_modifier_bits_are_kept() {
        let mut r = swap();
        assert_eq!(
            r.apply(mods(mask::SHIFT | mask::MOD1 | mask::MOD4)),
            vec![mods(mask::SHIFT | mask::MOD1 | mask::CONTROL)]
        );
    }

    #[test]
    fn one_way_remap_does_not_resurrect_the_source_bit() {
        // Command → Control only: nothing should still claim Super
        let mut r = KeyRemap::new(HashMap::from([(KeyLeftMeta, KeyLeftCtrl)]), Vec::new());
        assert_eq!(r.apply(mods(mask::MOD4)), vec![mods(mask::CONTROL)]);
    }

    #[test]
    fn empty_remap_is_a_passthrough() {
        let mut r = KeyRemap::default();
        assert!(r.is_empty());
        assert_eq!(
            r.apply(key_ev(KeyLeftMeta, 1)),
            vec![key_ev(KeyLeftMeta, 1)]
        );
        assert_eq!(r.apply(mods(mask::MOD4)), vec![mods(mask::MOD4)]);
    }

    #[test]
    fn remapping_a_non_modifier_leaves_masks_alone() {
        let mut r = KeyRemap::new(HashMap::from([(KeyA, KeyLeftCtrl)]), Vec::new());
        assert_eq!(r.apply(key_ev(KeyA, 1)), vec![key_ev(KeyLeftCtrl, 1)]);
        assert_eq!(r.apply(mods(mask::MOD4)), vec![mods(mask::MOD4)]);
    }

    #[test]
    fn chord_trigger_sends_override_instead_of_plain() {
        let mut r = swap_with_chord();
        // macOS pairs a modifier key event with a `Modifiers` update;
        // the latter is suppressed until the chord resolves
        assert_eq!(r.apply(key_ev(KeyLeftMeta, 1)), Vec::<Event>::new());
        assert_eq!(r.apply(mods(mask::MOD4)), Vec::<Event>::new());
        assert_eq!(
            r.apply(key_ev(KeyTab, 1)),
            vec![key_ev(KeyLeftAlt, 1), mods(mask::MOD1), key_ev(KeyTab, 1)]
        );
    }

    #[test]
    fn chord_release_sends_override_up() {
        let mut r = swap_with_chord();
        r.apply(key_ev(KeyLeftMeta, 1));
        r.apply(key_ev(KeyTab, 1));
        r.apply(key_ev(KeyTab, 0));
        assert_eq!(r.apply(key_ev(KeyLeftMeta, 0)), vec![key_ev(KeyLeftAlt, 0)]);
    }

    #[test]
    fn repeated_tab_taps_pass_through_unchanged_once_chord_is_active() {
        let mut r = swap_with_chord();
        r.apply(key_ev(KeyLeftMeta, 1));
        r.apply(key_ev(KeyTab, 1));
        assert_eq!(r.apply(key_ev(KeyTab, 0)), vec![key_ev(KeyTab, 0)]);
        assert_eq!(r.apply(key_ev(KeyTab, 1)), vec![key_ev(KeyTab, 1)]);
    }

    #[test]
    fn non_trigger_key_falls_back_to_plain_mapping() {
        // Cmd+C: not a chord, must still become Ctrl+C
        let mut r = swap_with_chord();
        assert_eq!(r.apply(key_ev(KeyLeftMeta, 1)), Vec::<Event>::new());
        assert_eq!(
            r.apply(key_ev(KeyC, 1)),
            vec![key_ev(KeyLeftCtrl, 1), mods(0), key_ev(KeyC, 1)]
        );
    }

    #[test]
    fn tapping_the_chord_modifier_alone_resolves_as_plain_on_release() {
        let mut r = swap_with_chord();
        assert_eq!(r.apply(key_ev(KeyLeftMeta, 1)), Vec::<Event>::new());
        assert_eq!(
            r.apply(key_ev(KeyLeftMeta, 0)),
            vec![key_ev(KeyLeftCtrl, 1), key_ev(KeyLeftCtrl, 0)]
        );
    }

    #[test]
    fn other_modifier_held_alongside_does_not_force_resolution() {
        // Cmd+Shift+Tab (cycle backwards) must still become Alt+Shift+Tab
        let mut r = swap_with_chord();
        r.apply(key_ev(KeyLeftMeta, 1));
        assert_eq!(
            r.apply(key_ev(KeyLeftShift, 1)),
            vec![key_ev(KeyLeftShift, 1)]
        );
        assert_eq!(
            r.apply(key_ev(KeyTab, 1)),
            vec![key_ev(KeyLeftAlt, 1), mods(0), key_ev(KeyTab, 1)]
        );
    }

    #[test]
    fn mouse_motion_does_not_disturb_a_pending_chord() {
        let mut r = swap_with_chord();
        r.apply(key_ev(KeyLeftMeta, 1));
        let motion = Event::Pointer(PointerEvent::Motion {
            time: 0,
            dx: 1.0,
            dy: 1.0,
        });
        assert_eq!(r.apply(motion), vec![motion]);
        assert_eq!(
            r.apply(key_ev(KeyTab, 1)),
            vec![key_ev(KeyLeftAlt, 1), mods(0), key_ev(KeyTab, 1)]
        );
    }

    #[test]
    fn mouse_click_resolves_a_pending_chord_as_plain() {
        // Cmd+Click must still become Ctrl+Click
        let mut r = swap_with_chord();
        r.apply(key_ev(KeyLeftMeta, 1));
        let click = Event::Pointer(PointerEvent::Button {
            time: 0,
            button: input_event::BTN_LEFT,
            state: 1,
        });
        assert_eq!(r.apply(click), vec![key_ev(KeyLeftCtrl, 1), mods(0), click]);
    }

    #[test]
    fn release_key_reports_the_override_while_a_chord_is_active() {
        let mut r = swap_with_chord();
        r.apply(key_ev(KeyLeftMeta, 1));
        r.apply(key_ev(KeyTab, 1));
        assert_eq!(r.release_key(KeyLeftMeta), Some(KeyLeftAlt));
    }

    #[test]
    fn release_key_reports_nothing_for_a_still_pending_modifier() {
        // no down was ever sent for it — a synthesized key-up would
        // release a key the peer was never told was pressed
        let mut r = swap_with_chord();
        r.apply(key_ev(KeyLeftMeta, 1));
        assert_eq!(r.release_key(KeyLeftMeta), None);
    }

    #[test]
    fn release_key_falls_back_to_the_plain_mapping_otherwise() {
        let mut r = swap_with_chord();
        assert_eq!(r.release_key(KeyLeftMeta), Some(KeyLeftCtrl));
        assert_eq!(r.release_key(KeyA), Some(KeyA));
    }
}
