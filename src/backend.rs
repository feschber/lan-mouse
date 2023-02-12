pub mod windows;
pub mod wayland;
pub mod x11;

#[derive(Clone, Copy, Debug)]
pub enum Backend {
    X11,
    WAYLAND,
}
