use crate::config::Config;
use clap::Args;
use futures::StreamExt;
use input_capture::{self, CaptureError, CaptureEvent, InputCapture, InputCaptureError, Position};
use input_event::{Event, KeyboardEvent};

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct TestCaptureArgs {}

pub async fn run(config: Config, _args: TestCaptureArgs) -> Result<(), InputCaptureError> {
    log::info!("running input capture test");
    log::info!("creating input capture");
    let backend = config.capture_backend().map(|b| b.into());
    loop {
        let mut input_capture = InputCapture::new(backend).await?;
        log::info!("creating clients");
        input_capture.create(0, Position::Left).await?;
        input_capture.create(4, Position::Left).await?;
        input_capture.create(1, Position::Right).await?;
        input_capture.create(2, Position::Top).await?;
        input_capture.create(3, Position::Bottom).await?;
        if let Err(e) = do_capture(&mut input_capture).await {
            log::warn!("{e} - recreating capture");
        }
        let _ = input_capture.terminate().await;
    }
}

async fn do_capture(input_capture: &mut InputCapture) -> Result<(), CaptureError> {
    loop {
        let (client, event) = input_capture
            .next()
            .await
            .ok_or(CaptureError::EndOfStream)??;
        let pos = match client {
            0 | 4 => Position::Left,
            1 => Position::Right,
            2 => Position::Top,
            3 => Position::Bottom,
            _ => panic!(),
        };
        log::info!("position: {client} ({pos}), event: {event}");
        if let CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key { key: 1, .. })) = event {
            input_capture.release().await?;
            break Ok(());
        }
    }
}
