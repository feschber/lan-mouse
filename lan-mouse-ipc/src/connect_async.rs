use crate::{ConnectionError, FrontendEvent, FrontendRequest, IpcError};
use std::{
    cmp::min,
    task::{Poll, ready},
    time::Duration,
};

use futures::{Stream, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio_stream::wrappers::LinesStream;

#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpStream;

pub struct AsyncFrontendEventReader {
    #[cfg(unix)]
    lines_stream: LinesStream<BufReader<ReadHalf<UnixStream>>>,
    #[cfg(windows)]
    lines_stream: LinesStream<BufReader<ReadHalf<TcpStream>>>,
}

pub struct AsyncFrontendRequestWriter {
    #[cfg(unix)]
    tx: WriteHalf<UnixStream>,
    #[cfg(windows)]
    tx: WriteHalf<TcpStream>,
}

impl Stream for AsyncFrontendEventReader {
    type Item = Result<FrontendEvent, IpcError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let line = ready!(self.lines_stream.poll_next_unpin(cx));
        let event = line.map(|l| {
            l.map_err(Into::<IpcError>::into)
                .and_then(|l| serde_json::from_str(l.as_str()).map_err(|e| e.into()))
        });
        Poll::Ready(event)
    }
}

impl AsyncFrontendRequestWriter {
    pub async fn request(&mut self, request: FrontendRequest) -> Result<(), IpcError> {
        let mut json = serde_json::to_string(&request).unwrap();
        log::debug!("requesting: {json}");
        json.push('\n');
        self.tx.write_all(json.as_bytes()).await?;
        Ok(())
    }
}

pub async fn connect_async(
    timeout: Option<Duration>,
) -> Result<(AsyncFrontendEventReader, AsyncFrontendRequestWriter), ConnectionError> {
    let stream = if let Some(duration) = timeout {
        tokio::select! {
            s = wait_for_service() => s?,
            _ = tokio::time::sleep(duration) => return Err(ConnectionError::Timeout),
        }
    } else {
        wait_for_service().await?
    };
    #[cfg(unix)]
    let (rx, tx): (ReadHalf<UnixStream>, WriteHalf<UnixStream>) = tokio::io::split(stream);
    #[cfg(windows)]
    let (rx, tx): (ReadHalf<TcpStream>, WriteHalf<TcpStream>) = tokio::io::split(stream);
    let buf_reader = BufReader::new(rx);
    let lines = buf_reader.lines();
    let lines_stream = LinesStream::new(lines);
    let reader = AsyncFrontendEventReader { lines_stream };
    let writer = AsyncFrontendRequestWriter { tx };
    Ok((reader, writer))
}

/// wait for the lan-mouse socket to come online
#[cfg(unix)]
async fn wait_for_service() -> Result<UnixStream, ConnectionError> {
    let socket_path = crate::default_socket_path()?;
    let mut duration = Duration::from_millis(10);
    loop {
        if let Ok(stream) = UnixStream::connect(&socket_path).await {
            break Ok(stream);
        }
        // a signaling mechanism or inotify could be used to
        // improve this
        tokio::time::sleep(exponential_back_off(&mut duration)).await;
    }
}

#[cfg(windows)]
async fn wait_for_service() -> Result<TcpStream, ConnectionError> {
    let mut duration = Duration::from_millis(10);
    loop {
        if let Ok(stream) = TcpStream::connect("127.0.0.1:5252").await {
            break Ok(stream);
        }
        tokio::time::sleep(exponential_back_off(&mut duration)).await;
    }
}

fn exponential_back_off(duration: &mut Duration) -> Duration {
    let new = duration.saturating_mul(2);
    *duration = min(new, Duration::from_secs(1));
    *duration
}
