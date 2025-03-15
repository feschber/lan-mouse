use crate::config::Config;
use clap::Args;
use input_emulation::{InputEmulation, InputEmulationError};
use input_event::{Event, PointerEvent};
use std::f64::consts::PI;
use std::time::{Duration, Instant};

const FREQUENCY_HZ: f64 = 1.0;
const RADIUS: f64 = 100.0;

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TestEmulationArgs {
    #[arg(long)]
    mouse: bool,
    #[arg(long)]
    keyboard: bool,
    #[arg(long)]
    scroll: bool,
}

pub async fn run(config: Config, _args: TestEmulationArgs) -> Result<(), InputEmulationError> {
    log::info!("running input emulation test");

    let backend = config.emulation_backend().map(|b| b.into());
    let mut emulation = InputEmulation::new(backend).await?;
    emulation.create(0).await;

    let start = Instant::now();
    let mut offset = (0, 0);
    loop {
        tokio::time::sleep(Duration::from_millis(1)).await;
        let elapsed = start.elapsed();
        let elapsed_sec_f64 = elapsed.as_secs_f64();
        let second_fraction = elapsed_sec_f64 - elapsed_sec_f64 as u64 as f64;
        let radians = second_fraction * 2. * PI * FREQUENCY_HZ;
        let new_offset_f = (radians.cos() * RADIUS * 2., (radians * 2.).sin() * RADIUS);
        let new_offset = (new_offset_f.0 as i32, new_offset_f.1 as i32);
        if new_offset != offset {
            let relative_motion = (new_offset.0 - offset.0, new_offset.1 - offset.1);
            offset = new_offset;
            let (dx, dy) = (relative_motion.0 as f64, relative_motion.1 as f64);
            let event = Event::Pointer(PointerEvent::Motion { time: 0, dx, dy });
            emulation.consume(event, 0).await?;
        }
    }
}
