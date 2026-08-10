use std::collections::HashMap;

use input_event::{Event, KeyboardEvent, scancode};

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

/// Rewrites keys on their way to another device.
///
/// The typical use is reconciling modifier layouts between operating
/// systems — sending Command from a Mac as Control, and Control as
/// Super, so muscle memory keeps working on a Windows or Linux peer.
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
}

impl KeyRemap {
    pub(crate) fn new(keys: HashMap<scancode::Linux, scancode::Linux>) -> Self {
        let bits = keys
            .iter()
            .filter(|(from, to)| from != to)
            .filter_map(|(&from, &to)| Some((modifier_bit(from)?, modifier_bit(to)?)))
            .filter(|(from, to)| from != to)
            .collect();
        Self { keys, bits }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// the key this one is sent as
    pub(crate) fn key(&self, key: scancode::Linux) -> scancode::Linux {
        self.keys.get(&key).copied().unwrap_or(key)
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

    /// Rewrite an outgoing event. Anything that isn't a key or a
    /// modifier update passes through untouched.
    pub(crate) fn apply(&self, event: Event) -> Event {
        if self.is_empty() {
            return event;
        }
        match event {
            Event::Keyboard(KeyboardEvent::Key { time, key, state }) => {
                let key = match scancode::Linux::try_from(key) {
                    Ok(k) => self.key(k) as u32,
                    Err(_) => key,
                };
                Event::Keyboard(KeyboardEvent::Key { time, key, state })
            }
            Event::Keyboard(KeyboardEvent::Modifiers {
                depressed,
                latched,
                locked,
                group,
            }) => Event::Keyboard(KeyboardEvent::Modifiers {
                depressed: self.remap_mask(depressed),
                latched: self.remap_mask(latched),
                locked: self.remap_mask(locked),
                group,
            }),
            e => e,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use input_event::scancode::Linux::{KeyA, KeyLeftAlt, KeyLeftCtrl, KeyLeftMeta};

    /// the swap this exists for: Command ⇄ Control
    fn swap() -> KeyRemap {
        KeyRemap::new(HashMap::from([
            (KeyLeftMeta, KeyLeftCtrl),
            (KeyLeftCtrl, KeyLeftMeta),
        ]))
    }

    fn key_event(key: scancode::Linux, state: u8) -> Event {
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
        let r = swap();
        assert_eq!(
            r.apply(key_event(KeyLeftMeta, 1)),
            key_event(KeyLeftCtrl, 1)
        );
        assert_eq!(
            r.apply(key_event(KeyLeftCtrl, 1)),
            key_event(KeyLeftMeta, 1)
        );
    }

    #[test]
    fn leaves_unmapped_keys_alone() {
        let r = swap();
        assert_eq!(r.apply(key_event(KeyA, 1)), key_event(KeyA, 1));
        assert_eq!(r.apply(key_event(KeyLeftAlt, 1)), key_event(KeyLeftAlt, 1));
    }

    #[test]
    fn key_state_is_preserved() {
        let r = swap();
        assert_eq!(
            r.apply(key_event(KeyLeftMeta, 0)),
            key_event(KeyLeftCtrl, 0)
        );
    }

    #[test]
    fn modifier_mask_swaps_with_the_keys() {
        // the whole point: the mask must agree with the key events,
        // or the peer holds Control while its mask says Super
        let r = swap();
        assert_eq!(r.apply(mods(mask::MOD4)), mods(mask::CONTROL));
        assert_eq!(r.apply(mods(mask::CONTROL)), mods(mask::MOD4));
    }

    #[test]
    fn both_modifiers_held_survives_the_swap() {
        // a plain "clear then set" that forgot to clear first would
        // drop one of the two bits here
        let r = swap();
        assert_eq!(
            r.apply(mods(mask::CONTROL | mask::MOD4)),
            mods(mask::CONTROL | mask::MOD4)
        );
    }

    #[test]
    fn untouched_modifier_bits_are_kept() {
        let r = swap();
        assert_eq!(
            r.apply(mods(mask::SHIFT | mask::MOD1 | mask::MOD4)),
            mods(mask::SHIFT | mask::MOD1 | mask::CONTROL)
        );
    }

    #[test]
    fn one_way_remap_does_not_resurrect_the_source_bit() {
        // Command → Control only: nothing should still claim Super
        let r = KeyRemap::new(HashMap::from([(KeyLeftMeta, KeyLeftCtrl)]));
        assert_eq!(r.apply(mods(mask::MOD4)), mods(mask::CONTROL));
    }

    #[test]
    fn empty_remap_is_a_passthrough() {
        let r = KeyRemap::default();
        assert!(r.is_empty());
        assert_eq!(
            r.apply(key_event(KeyLeftMeta, 1)),
            key_event(KeyLeftMeta, 1)
        );
        assert_eq!(r.apply(mods(mask::MOD4)), mods(mask::MOD4));
    }

    #[test]
    fn remapping_a_non_modifier_leaves_masks_alone() {
        let r = KeyRemap::new(HashMap::from([(KeyA, KeyLeftCtrl)]));
        assert_eq!(r.apply(key_event(KeyA, 1)), key_event(KeyLeftCtrl, 1));
        assert_eq!(r.apply(mods(mask::MOD4)), mods(mask::MOD4));
    }
}
