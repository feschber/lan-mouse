#[cfg(windows)]
pub mod windows;

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
pub mod x11;

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
pub mod wlroots;

#[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
pub mod xdg_desktop_portal;

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
pub mod libei;

#[cfg(target_os = "macos")]
pub mod macos;

/// fallback consumer
pub mod dummy;
