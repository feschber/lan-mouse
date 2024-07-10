use std::{fmt::Display, io};

use futures_core::Stream;

use input_event::Event;

pub use error::{CaptureCreationError, CaptureError};

pub mod error;

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
pub mod libei;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
pub mod wayland;

#[cfg(windows)]
pub mod windows;

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
pub mod x11;

/// fallback input capture (does not produce events)
pub mod dummy;

pub type CaptureHandle = u64;

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

impl Position {
    pub fn opposite(&self) -> Self {
        match self {
            Position::Left => Self::Right,
            Position::Right => Self::Left,
            Position::Top => Self::Bottom,
            Position::Bottom => Self::Top,
        }
    }
}

impl Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pos = match self {
            Position::Left => "left",
            Position::Right => "right",
            Position::Top => "top",
            Position::Bottom => "bottom",
        };
        write!(f, "{}", pos)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    InputCapturePortal,
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    LayerShell,
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            Backend::InputCapturePortal => write!(f, "input-capture-portal"),
            #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
            Backend::LayerShell => write!(f, "layer-shell"),
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
            Backend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            Backend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            Backend::MacOs => write!(f, "MacOS"),
            Backend::Dummy => write!(f, "dummy"),
        }
    }
}

pub trait InputCapture:
    Stream<Item = Result<(CaptureHandle, Event), CaptureError>> + Unpin
{
    /// create a new client with the given id
    fn create(&mut self, id: CaptureHandle, pos: Position) -> io::Result<()>;

    /// destroy the client with the given id, if it exists
    fn destroy(&mut self, id: CaptureHandle) -> io::Result<()>;

    /// release mouse
    fn release(&mut self) -> io::Result<()>;
}

pub async fn create_backend(
    backend: Backend,
) -> Result<
    Box<dyn InputCapture<Item = Result<(CaptureHandle, Event), CaptureError>>>,
    CaptureCreationError,
> {
    match backend {
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        Backend::InputCapturePortal => Ok(Box::new(libei::LibeiInputCapture::new().await?)),
        #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
        Backend::LayerShell => Ok(Box::new(wayland::WaylandInputCapture::new()?)),
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        Backend::X11 => Ok(Box::new(x11::X11InputCapture::new()?)),
        #[cfg(windows)]
        Backend::Windows => Ok(Box::new(windows::WindowsInputCapture::new())),
        #[cfg(target_os = "macos")]
        Backend::MacOs => Ok(Box::new(macos::MacOSInputCapture::new()?)),
        Backend::Dummy => Ok(Box::new(dummy::DummyInputCapture::new())),
    }
}

pub async fn create(
    backend: Option<Backend>,
) -> Result<
    Box<dyn InputCapture<Item = Result<(CaptureHandle, Event), CaptureError>>>,
    CaptureCreationError,
> {
    if let Some(backend) = backend {
        let b = create_backend(backend).await;
        if b.is_ok() {
            log::info!("using capture backend: {backend}");
        }
        return b;
    }

    for backend in [
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        Backend::InputCapturePortal,
        #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
        Backend::LayerShell,
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        Backend::X11,
        #[cfg(windows)]
        Backend::Windows,
        #[cfg(target_os = "macos")]
        Backend::MacOs,
        Backend::Dummy,
    ] {
        match create_backend(backend).await {
            Ok(b) => {
                log::info!("using capture backend: {backend}");
                return Ok(b);
            }
            Err(e) => log::warn!("{backend} input capture backend unavailable: {e}"),
        }
    }
    Err(CaptureCreationError::NoAvailableBackend)
}
