//! Reproduction for issue #478: "Too many references: cannot splice (os error 109)",
//! which is ETOOMANYREFS, on a wlroots compositor.
//!
//! `State::add_client` creates a virtual keyboard per emulation client and hands the
//! compositor a keymap file descriptor. It is the same descriptor every time, and
//! Wayland caps descriptors per sendmsg (MAX_FDS_OUT), so under client churn they
//! queue faster than the compositor drains them until the send is refused.
//!
//! Run under a wlroots compositor:
//!   cargo run --release -p input-emulation --example keymap_fd_churn
//!   cargo run --release -p input-emulation --example keymap_fd_churn -- --control
//!
//! Churn mode creates one client per iteration. Control mode holds the client count
//! at one and sends the same number of events, which is what distinguishes client
//! creation from the event path.
//!
//! The descriptor limit matters. `too_many_unix_fds()` compares the USER's total
//! in-flight SCM_RIGHTS count against the SENDER's RLIMIT_NOFILE, so a limit below
//! the machine's ambient in-flight count fails at iteration 0 regardless of this bug,
//! and a limit far above it never trips. On the machine this was written on, ambient
//! sits near 1024 and `ulimit -n 2048` fails at about iteration 900 before the fix and
//! runs clean after it.
//!
//! Motion events are sent with dx and dy of zero so the example never moves the real
//! cursor.

use input_emulation::{Backend, EmulationHandle, InputEmulation};
use input_event::{Event, PointerEvent};

/// number of simulated peer reconnects
const ITERATIONS: u64 = 2000;

/// how often to sample the descriptor count
const SAMPLE_EVERY: u64 = 100;

/// counts open descriptors of this process.
/// returns None rather than 0 when the count itself fails for lack of a descriptor,
/// so an exhausted process is never reported as using none.
fn open_fds() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd").ok().map(|d| d.count())
}

fn fds_display(fds: Option<usize>) -> String {
    match fds {
        Some(n) => n.to_string(),
        None => "unreadable (process out of descriptors)".to_string(),
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build runtime");

    runtime.block_on(async {
        let mut emulation = match InputEmulation::new(Some(Backend::Wlroots)).await {
            Ok(e) => e,
            Err(e) => {
                eprintln!("could not create wlroots emulation backend: {e}");
                eprintln!("this example requires a running wlroots compositor");
                std::process::exit(1);
            }
        };

        // control mode reuses a single client, so only the event path runs.
        // if descriptors stay flat here but climb in the default mode, client
        // creation is the source and the event path is exonerated.
        let control = std::env::args().any(|a| a == "--control");

        let baseline = open_fds().unwrap_or(0);
        println!("mode: {}", if control { "control (one client, N events)" } else { "churn (N clients)" });
        println!("baseline open fds: {baseline}");
        println!("running {ITERATIONS} iterations");

        if control {
            emulation.create(0).await;
        }

        for i in 0..ITERATIONS {
            let handle = if control { 0 } else { i as EmulationHandle };

            if !control {
                // mirrors do_emulation_session: an unseen peer address creates a client
                emulation.create(handle).await;
            }

            // a zero motion event is harmless but forces a flush of the queued keymap fd
            let event = Event::Pointer(PointerEvent::Motion {
                time: 0,
                dx: 0.0,
                dy: 0.0,
            });

            if let Err(e) = emulation.consume(event, handle).await {
                println!();
                println!("FAILED at iteration {i}");
                println!("open fds: {} (baseline {baseline})", fds_display(open_fds()));
                println!("error: {e}");
                std::process::exit(1);
            }

            if i % SAMPLE_EVERY == 0 {
                let now = open_fds();
                let delta = now.map(|n| n.saturating_sub(baseline));
                println!(
                    "  iteration {i:>5}: open fds {:>6}{}",
                    fds_display(now),
                    delta.map(|d| format!(" (+{d} over baseline)")).unwrap_or_default(),
                );
            }
        }

        let final_fds = open_fds();
        println!();
        println!("completed {ITERATIONS} iterations without error");
        println!(
            "open fds: {} (baseline {baseline})",
            fds_display(final_fds)
        );
        emulation.terminate().await;
    });
}
