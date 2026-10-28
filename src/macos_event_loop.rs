use super::LanMouseError;
use dispatch2::MainThreadBound;
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
};
use objc2_foundation::NSPoint;
use std::{io, thread};

struct StopApplication {
    application: Retained<NSApplication>,
    wake_event: Retained<NSEvent>,
}

impl Drop for StopApplication {
    fn drop(&mut self) {
        self.application.stop(None);
        // stop() alone does not unblock AppKit's wait for the next event.
        self.application.postEvent_atStart(&self.wake_event, true);
    }
}

pub(super) fn run(
    run_runtime: impl FnOnce() -> Result<(), LanMouseError> + Send,
) -> Result<(), LanMouseError> {
    let main_thread = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("AppKit event loop requires the main thread"))?;
    let application = NSApplication::sharedApplication(main_thread);
    if application.activationPolicy() != NSApplicationActivationPolicy::Prohibited
        && !application.setActivationPolicy(NSApplicationActivationPolicy::Prohibited)
    {
        return Err(
            io::Error::other("could not configure the background AppKit application").into(),
        );
    }
    let wake_event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    )
    .ok_or_else(|| io::Error::other("could not create the AppKit shutdown event"))?;
    application.finishLaunching();
    let stop = MainThreadBound::new(
        StopApplication {
            application: application.clone(),
            wake_event,
        },
        main_thread,
    );

    thread::scope(|scope| {
        let worker = thread::Builder::new().spawn_scoped(scope, move || {
            // MainThreadBound schedules cleanup on the main thread, including
            // when runtime creation fails or a debug build unwinds here.
            let _stop = stop;
            run_runtime()
        })?;
        // Display notifications require AppKit event dispatch; a CFRunLoop on
        // the event-tap thread alone does not service external layout changes.
        application.run();
        worker
            .join()
            .map_err(|_| io::Error::other("macOS async runtime thread panicked"))?
    })
}
