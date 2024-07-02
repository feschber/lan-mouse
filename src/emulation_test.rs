use crate::config::Config;
use anyhow::Result;
use input_event::{Event, PointerEvent};
use std::f64::consts::PI;
use std::time::{Duration, Instant};
use tokio::task::LocalSet;

pub fn run() -> Result<()> {
    log::info!("running input emulation test");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    let config = Config::new()?;

    runtime.block_on(LocalSet::new().run_until(input_emulation_test(config)))
}

const FREQUENCY_HZ: f64 = 1.0;
const RADIUS: f64 = 100.0;

async fn input_emulation_test(config: Config) -> Result<()> {
    let backend = config.emulation_backend.map(|b| b.into());
    let mut emulation = input_emulation::create(backend).await?;
    emulation.create(0).await;
    let start = Instant::now();
    let mut offset = (0, 0);
    loop {
        tokio::select! {
            _ = emulation.dispatch() => {}
            _ = tokio::time::sleep(Duration::from_millis(1)) => {
                let elapsed = start.elapsed();
                let elapsed_sec_f64 = elapsed.as_secs_f64();
                let second_fraction = elapsed_sec_f64 - elapsed_sec_f64 as u64 as f64;
                let radians = second_fraction * 2. * PI * FREQUENCY_HZ;
                let new_offset_f = (radians.cos() * RADIUS * 2., (radians * 2.).sin() * RADIUS);
                let new_offset = (new_offset_f.0 as i32, new_offset_f.1 as i32);
                if new_offset != offset {
                    let relative_motion = (new_offset.0 - offset.0, new_offset.1 - offset.1);
                    offset = new_offset;
                    let (relative_x, relative_y) = (relative_motion.0 as f64, relative_motion.1 as f64);
                    emulation.consume(Event::Pointer(PointerEvent::Motion {time: 0, relative_x, relative_y }), 0).await;
                }
            }
        }
    }
}
