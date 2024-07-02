use crate::config::Config;
use anyhow::{anyhow, Result};
use futures::StreamExt;
use input_capture::{self, Position};
use input_event::{Event, KeyboardEvent};
use tokio::task::LocalSet;

pub fn run() -> Result<()> {
    log::info!("running input capture test");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    let config = Config::new()?;

    runtime.block_on(LocalSet::new().run_until(input_capture_test(config)))
}

async fn input_capture_test(config: Config) -> Result<()> {
    log::info!("creating input capture");
    let backend = config.capture_backend.map(|b| b.into());
    let mut input_capture = input_capture::create(backend).await?;
    log::info!("creating clients");
    input_capture.create(0, Position::Left)?;
    input_capture.create(1, Position::Right)?;
    input_capture.create(2, Position::Top)?;
    input_capture.create(3, Position::Bottom)?;
    loop {
        let (client, event) = input_capture
            .next()
            .await
            .ok_or(anyhow!("capture stream closed"))??;
        let pos = match client {
            0 => Position::Left,
            1 => Position::Right,
            2 => Position::Top,
            _ => Position::Bottom,
        };
        log::info!("position: {pos}, event: {event}");
        if let Event::Keyboard(KeyboardEvent::Key { key: 1, .. }) = event {
            input_capture.release()?;
        }
    }
}
