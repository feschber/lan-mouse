//! Reproduction for double clicks not being recognized on a wlroots compositor.
//!
//! Clients detect double clicks by comparing event timestamps, so the timestamps
//! handed to the compositor must be milliseconds on a monotonic local clock.
//! The peer's capture backend decides what arrives over the network: libei sends
//! microseconds truncated to u32, Windows and macOS send 0.
//!
//! Run under a wlroots compositor with a client that reports click counts
//! (e.g. a GTK window listening for double-button-press):
//!   cargo run -p input-emulation --example double_click
//!
//! Two clicks 150ms apart are sent with libei-style timestamps.

#[cfg(wlroots)]
use input_emulation::{Backend, InputEmulation};
#[cfg(wlroots)]
use input_event::{BTN_LEFT, Event, PointerEvent};
#[cfg(wlroots)]
use std::time::{Duration, Instant};

#[cfg(wlroots)]
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build runtime");

    runtime.block_on(async {
        let mut emulation = InputEmulation::new(Some(Backend::Wlroots))
            .await
            .expect("this example requires a running wlroots compositor");
        emulation.create(0).await;

        let start = Instant::now();
        // libei timestamps: microseconds, truncated to u32 by the capture side
        let peer_time = || start.elapsed().as_micros() as u32;

        // give the pointer focus to the surface under it before clicking
        let motion = Event::Pointer(PointerEvent::Motion {
            time: peer_time(),
            dx: 1.,
            dy: 1.,
        });
        emulation.consume(motion, 0).await.expect("consume");
        tokio::time::sleep(Duration::from_millis(200)).await;

        for _ in 0..2 {
            for state in [1, 0] {
                let time = peer_time();
                println!("button state={state} peer time={time}");
                let event = Event::Pointer(PointerEvent::Button {
                    time,
                    button: BTN_LEFT,
                    state,
                });
                emulation.consume(event, 0).await.expect("consume");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        emulation.terminate().await;
    });
}

#[cfg(not(wlroots))]
fn main() {
    eprintln!("this example requires the wlroots feature");
}
