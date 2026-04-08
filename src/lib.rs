mod capture;
pub mod capture_test;
pub mod client;
pub mod config;
mod connect;
mod crypto;
mod dns;
mod emulation;
pub mod emulation_test;
mod listen;
pub mod service;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub mod windows_service;

#[cfg(windows)]
static IS_WINDOWS_SERVICE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
pub fn set_is_windows_service(is_service: bool) {
    IS_WINDOWS_SERVICE.store(is_service, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(windows)]
pub fn is_windows_service() -> bool {
    IS_WINDOWS_SERVICE.load(std::sync::atomic::Ordering::SeqCst)
}
