#[cfg(windows)]
pub mod windows;

#[cfg(all(unix, feature="x11"))]
pub mod x11;

#[cfg(all(unix, feature = "wayland"))]
pub mod wlroots;

#[cfg(all(unix, feature = "xdg_desktop_portal"))]
pub mod xdg_desktop_portal;

#[cfg(all(unix, feature = "libei"))]
pub mod libei;
