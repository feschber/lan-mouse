#[cfg(windows)]
pub mod windows;

#[cfg(feature="x11")]
pub mod x11;

#[cfg(feature = "wayland")]
pub mod wlroots;

#[cfg(feature = "xdg_desktop_portal")]
pub mod xdg_desktop_portal;

#[cfg(feature = "libei")]
pub mod libei;
