#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
pub mod wayland;

#[cfg(unix)]
pub mod x11;

#[derive(Clone, Copy, Debug)]
pub enum Backend {
    X11,
    WAYLAND,
}
