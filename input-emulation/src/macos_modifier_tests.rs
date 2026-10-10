use super::*;
use std::{cell::RefCell, future::Future};
#[derive(Clone, Debug)]
struct Observed {
    kind: u32,
    key: i64,
    flags: CGEventFlags,
}
thread_local! { static EVENTS: RefCell<Vec<Observed>> = const { RefCell::new(Vec::new()) }; }
pub(super) fn record(e: &CGEvent) {
    EVENTS.with(|v| {
        v.borrow_mut().push(Observed {
            kind: e.get_type() as u32,
            key: e.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
            flags: e.get_flags(),
        })
    });
}
fn events() -> Vec<Observed> {
    EVENTS.with(|v| v.borrow().clone())
}
fn clear() {
    EVENTS.with(|v| v.borrow_mut().clear());
}
// Constructs an in-memory backend. post_event() is an observation sink in tests.
// No system permissions are requested and no keyboard/pointer events are injected.
fn backend() -> MacOSEmulation {
    MacOSEmulation {
        event_source: CGEventSource::new(CGEventSourceStateID::Private).unwrap(),
        repeat_task: None,
        repeat_cancel: None,
        repeat_key: None,
        pressed_normal_keys: HashSet::new(),
        pressed_modifier_keys: HashSet::new(),
        pressed_buttons: HashSet::new(),
        previous_button: None,
        previous_button_click: None,
        button_click_state: 0,
        modifier_state: Rc::new(Cell::new(XMods::empty())),
    }
}
fn run<F, Fut>(f: F)
where
    F: FnOnce(MacOSEmulation) -> Fut,
    Fut: Future<Output = ()>,
{
    clear();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        tokio::time::timeout(Duration::from_secs(2), f(backend()))
            .await
            .expect("key processing stalled");
    });
}
async fn key(e: &mut MacOSEmulation, key: u32, state: u8) {
    e.consume(
        Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key,
            state,
        }),
        0,
    )
    .await
    .unwrap();
}
async fn mods(e: &mut MacOSEmulation, depressed: u32, latched: u32, locked: u32) {
    e.consume(
        Event::Keyboard(KeyboardEvent::Modifiers {
            depressed,
            latched,
            locked,
            group: 0,
        }),
        0,
    )
    .await
    .unwrap();
}
#[test]
fn regression_shift_has_real_keycode() {
    run(|mut e| async move {
        key(&mut e, 42, 1).await;
        let out = events();
        assert_eq!(
            out.len(),
            1,
            "one real FlagsChanged, no phantom A or regular modifier keyDown"
        );
        assert_eq!(out[0].kind, CGEventType::FlagsChanged as u32);
        assert_eq!(out[0].key, 56);
        assert!(out[0].flags.contains(CGEventFlags::CGEventFlagShift));
    });
}
#[test]
fn regression_snapshot_does_not_emit_a() {
    run(|mut e| async move {
        mods(&mut e, 1, 0, 0).await;
        assert!(
            events().is_empty(),
            "state-only snapshot must not post a keycode-zero event"
        );
    });
}
#[test]
fn regression_modifiers_never_repeat() {
    run(|mut e| async move {
        key(&mut e, 42, 1).await;
        tokio::time::sleep(Duration::from_millis(620)).await;
        assert_eq!(
            events().len(),
            1,
            "holding Shift must not enter the normal-key repeat task"
        );
    });
}
#[test]
fn regression_modifier_release_does_not_release_letter() {
    run(|mut e| async move {
        key(&mut e, 42, 1).await;
        key(&mut e, 45, 1).await;
        clear();
        key(&mut e, 42, 0).await;
        assert!(
            !events()
                .iter()
                .any(|v| v.key == 7 && v.kind == CGEventType::KeyUp as u32),
            "releasing Shift must not release held X"
        );
    });
}

#[test]
fn every_modifier_uses_its_own_keycode_on_down_and_up() {
    run(|mut e| async move {
        for (linux, mac, flag) in [
            (42, 56, CGEventFlags::CGEventFlagShift),
            (54, 60, CGEventFlags::CGEventFlagShift),
            (29, 59, CGEventFlags::CGEventFlagControl),
            (97, 62, CGEventFlags::CGEventFlagControl),
            (56, 58, CGEventFlags::CGEventFlagAlternate),
            (100, 61, CGEventFlags::CGEventFlagAlternate),
            (125, 55, CGEventFlags::CGEventFlagCommand),
            (126, 54, CGEventFlags::CGEventFlagCommand),
        ] {
            clear();
            key(&mut e, linux, 1).await;
            key(&mut e, linux, 0).await;
            let out = events();
            assert_eq!(out.len(), 2);
            assert!(
                out.iter()
                    .all(|v| v.kind == CGEventType::FlagsChanged as u32 && v.key == mac)
            );
            assert!(out[0].flags.contains(flag));
            assert!(!out[1].flags.contains(flag));
            assert!(e.repeat_task.is_none());
        }
    });
}
#[test]
fn shift_plus_letter_is_uppercase_then_released() {
    run(|mut e| async move {
        key(&mut e, 42, 1).await;
        mods(&mut e, 1, 0, 0).await;
        key(&mut e, 45, 1).await;
        key(&mut e, 45, 0).await;
        key(&mut e, 42, 0).await;
        mods(&mut e, 0, 0, 0).await;
        let out = events();
        assert_eq!(out.len(), 4);
        assert!(out[1].key == 7 && out[1].flags.contains(CGEventFlags::CGEventFlagShift));
        assert!(out[2].key == 7 && out[2].kind == CGEventType::KeyUp as u32);
        assert!(out[3].flags.is_empty());
        assert!(e.pressed_normal_keys.is_empty());
        assert!(!out.iter().any(|v| v.key == 0));
    });
}
#[test]
fn multiple_modifiers_keep_a_coherent_chord() {
    run(|mut e| async move {
        for k in [29, 56, 42] {
            key(&mut e, k, 1).await;
        }
        key(&mut e, 45, 1).await;
        let out = events();
        let last = out.last().unwrap();
        assert!(last.flags.contains(
            CGEventFlags::CGEventFlagControl
                | CGEventFlags::CGEventFlagAlternate
                | CGEventFlags::CGEventFlagShift
        ));
        assert_eq!(
            out.iter()
                .filter(|v| v.kind == CGEventType::KeyUp as u32)
                .count(),
            0
        );
        key(&mut e, 45, 0).await;
        for k in [42, 56, 29] {
            key(&mut e, k, 0).await;
        }
        assert!(e.modifier_state.get().is_empty());
    });
}
#[test]
fn left_and_right_shift_are_independent() {
    run(|mut e| async move {
        key(&mut e, 42, 1).await;
        key(&mut e, 54, 1).await;
        key(&mut e, 42, 0).await;
        assert!(
            events()
                .last()
                .unwrap()
                .flags
                .contains(CGEventFlags::CGEventFlagShift)
        );
        key(&mut e, 54, 0).await;
        assert!(events().last().unwrap().flags.is_empty());
    });
}
#[test]
fn caps_lock_remains_locked_on_release() {
    run(|mut e| async move {
        key(&mut e, 58, 1).await;
        key(&mut e, 58, 0).await;
        assert!(e.modifier_state.get().contains(XMods::LockMask));
        assert!(events().iter().all(|v| v.key == 57));
        key(&mut e, 58, 1).await;
        key(&mut e, 58, 0).await;
        assert!(!e.modifier_state.get().contains(XMods::LockMask));
    });
}
#[test]
fn caps_lock_snapshot_is_state_only() {
    run(|mut e| async move {
        mods(&mut e, 0, 0, 2).await;
        assert!(events().is_empty());
        assert!(e.modifier_state.get().contains(XMods::LockMask));
    });
}
#[test]
fn altgr_and_latched_shift_are_not_lost() {
    run(|mut e| async move {
        mods(&mut e, 128, 1, 0).await;
        assert!(events().is_empty());
        key(&mut e, 45, 1).await;
        let last = events().last().unwrap().clone();
        assert!(
            last.flags
                .contains(CGEventFlags::CGEventFlagAlternate | CGEventFlags::CGEventFlagShift)
        );
    });
}
#[test]
fn unknown_xkb_bits_do_not_clear_valid_modifiers() {
    run(|mut e| async move {
        mods(&mut e, (1 << 20) | 1, 0, 0).await;
        assert!(e.modifier_state.get().contains(XMods::ShiftMask));
    });
}
#[test]
fn fast_regular_key_down_up_never_loses_cancel_signal() {
    run(|mut e| async move {
        for _ in 0..20 {
            key(&mut e, 45, 1).await;
            key(&mut e, 45, 0).await;
        }
        assert_eq!(events().len(), 40);
        assert!(e.repeat_task.is_none());
    });
}
#[test]
fn releasing_other_held_key_does_not_stop_current_repeat() {
    run(|mut e| async move {
        key(&mut e, 45, 1).await;
        key(&mut e, 21, 1).await;
        clear();
        key(&mut e, 45, 0).await;
        assert_eq!(e.repeat_key, Some(16));
        assert_eq!(events().len(), 1);
        assert_eq!(events()[0].key, 7);
        key(&mut e, 21, 0).await;
        assert!(e.repeat_task.is_none());
    });
}
#[test]
fn normal_key_repeat_stops_on_actual_release() {
    run(|mut e| async move {
        key(&mut e, 45, 1).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(events().len() > 1);
        key(&mut e, 45, 0).await;
        let n = events().len();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(events().len(), n);
        assert_eq!(events().last().unwrap().kind, CGEventType::KeyUp as u32);
    });
}
#[test]
fn letter_a_still_works_as_a_real_key() {
    run(|mut e| async move {
        key(&mut e, 30, 1).await;
        key(&mut e, 30, 0).await;
        let out = events();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].key, 0);
        assert_eq!(out[0].kind, CGEventType::KeyDown as u32);
        assert_eq!(out[1].kind, CGEventType::KeyUp as u32);
    });
}
#[test]
fn command_arrow_keeps_navigation_flags() {
    run(|mut e| async move {
        key(&mut e, 125, 1).await;
        key(&mut e, 108, 1).await;
        assert!(events().last().unwrap().flags.contains(
            CGEventFlags::CGEventFlagCommand
                | CGEventFlags::CGEventFlagNumericPad
                | CGEventFlags::CGEventFlagSecondaryFn
        ));
    });
}
#[test]
fn terminate_releases_keys_without_emitting_phantom_a() {
    run(|mut e| async move {
        key(&mut e, 42, 1).await;
        key(&mut e, 45, 1).await;
        clear();
        e.terminate().await;
        assert!(
            events()
                .iter()
                .any(|v| v.key == 7 && v.kind == CGEventType::KeyUp as u32)
        );
        assert!(
            events()
                .iter()
                .any(|v| v.key == 56 && v.kind == CGEventType::FlagsChanged as u32)
        );
        assert!(!events().iter().any(|v| v.key == 0));
        assert!(
            e.repeat_task.is_none()
                && e.pressed_modifier_keys.is_empty()
                && e.pressed_normal_keys.is_empty()
        );
    });
}
#[test]
fn duplicate_modifier_does_not_toggle_or_repeat() {
    run(|mut e| async move {
        key(&mut e, 58, 1).await;
        key(&mut e, 58, 1).await;
        assert_eq!(events().len(), 1);
        assert!(e.modifier_state.get().contains(XMods::LockMask));
    });
}
