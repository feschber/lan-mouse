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
static IS_SERVICE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
pub fn set_is_service(is_service: bool) {
    IS_SERVICE.store(is_service, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(windows)]
pub fn is_service() -> bool {
    IS_SERVICE.load(std::sync::atomic::Ordering::SeqCst)
}
