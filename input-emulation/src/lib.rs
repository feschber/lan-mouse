use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use input_event::{Event, KeyboardEvent, PointerEvent};

pub use self::error::{EmulationCreationError, EmulationError, InputEmulationError};

#[cfg(windows)]
mod windows;

#[cfg(x11)]
mod x11;

#[cfg(wlroots)]
mod wlroots;

#[cfg(rdp)]
mod xdg_desktop_portal;

#[cfg(libei)]
mod libei;

#[cfg(target_os = "macos")]
mod macos;

/// fallback input emulation (logs events)
mod dummy;
mod error;

pub type EmulationHandle = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(wlroots)]
    Wlroots,
    #[cfg(libei)]
    Libei,
    #[cfg(rdp)]
    Xdp,
    #[cfg(x11)]
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
            #[cfg(wlroots)]
            Backend::Wlroots => write!(f, "wlroots"),
            #[cfg(libei)]
            Backend::Libei => write!(f, "libei"),
            #[cfg(rdp)]
            Backend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(x11)]
            Backend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            Backend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            Backend::MacOs => write!(f, "macos"),
            Backend::Dummy => write!(f, "dummy"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputConfig {
    pub invert_scroll: bool,
    pub mouse_sensitivity: f64,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            invert_scroll: false,
            mouse_sensitivity: 1.0,
        }
    }
}

fn post_process_event(event: Event, config: InputConfig) -> Event {
    match event {
        Event::Pointer(PointerEvent::Motion { time, dx, dy }) => {
            Event::Pointer(PointerEvent::Motion {
                time,
                dx: dx * config.mouse_sensitivity,
                dy: dy * config.mouse_sensitivity,
            })
        }
        Event::Pointer(PointerEvent::AxisDiscrete120 { axis, value }) if config.invert_scroll => {
            Event::Pointer(PointerEvent::AxisDiscrete120 {
                axis,
                value: -value,
            })
        }
        Event::Pointer(PointerEvent::Axis { time, axis, value }) if config.invert_scroll => {
            Event::Pointer(PointerEvent::Axis {
                time,
                axis,
                value: -value,
            })
        }
        _ => event,
    }
}

pub struct InputEmulation {
    emulation: Box<dyn Emulation>,
    handles: HashSet<EmulationHandle>,
    input_config: InputConfig,
    pressed_keys: HashMap<EmulationHandle, HashSet<u32>>,
}

impl InputEmulation {
    async fn with_backend(
        backend: Backend,
        input_config: InputConfig,
    ) -> Result<InputEmulation, EmulationCreationError> {
        let emulation: Box<dyn Emulation> = match backend {
            #[cfg(wlroots)]
            Backend::Wlroots => Box::new(wlroots::WlrootsEmulation::new()?),
            #[cfg(libei)]
            Backend::Libei => Box::new(libei::LibeiEmulation::new().await?),
            #[cfg(x11)]
            Backend::X11 => Box::new(x11::X11Emulation::new()?),
            #[cfg(rdp)]
            Backend::Xdp => Box::new(xdg_desktop_portal::DesktopPortalEmulation::new().await?),
            #[cfg(windows)]
            Backend::Windows => Box::new(windows::WindowsEmulation::new()?),
            #[cfg(target_os = "macos")]
            Backend::MacOs => Box::new(macos::MacOSEmulation::new()?),
            Backend::Dummy => Box::new(dummy::DummyEmulation::new()),
        };
        Ok(Self {
            emulation,
            input_config,
            handles: HashSet::new(),
            pressed_keys: HashMap::new(),
        })
    }

    pub async fn new(
        backend: Option<Backend>,
        input_config: InputConfig,
    ) -> Result<InputEmulation, EmulationCreationError> {
        if let Some(backend) = backend {
            let b = Self::with_backend(backend, input_config).await;
            if b.is_ok() {
                log::info!("using emulation backend: {backend}");
            }
            return b;
        }

        for backend in [
            #[cfg(wlroots)]
            Backend::Wlroots,
            #[cfg(libei)]
            Backend::Libei,
            #[cfg(rdp)]
            Backend::Xdp,
            #[cfg(x11)]
            Backend::X11,
            #[cfg(windows)]
            Backend::Windows,
            #[cfg(target_os = "macos")]
            Backend::MacOs,
            Backend::Dummy,
        ] {
            match Self::with_backend(backend, input_config).await {
                Ok(b) => {
                    log::info!("using emulation backend: {backend}");
                    return Ok(b);
                }
                Err(e) if e.cancelled_by_user() => return Err(e),
                Err(e) => log::warn!("{e}"),
            }
        }

        Err(EmulationCreationError::NoAvailableBackend)
    }

    pub async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        let event = post_process_event(event, self.input_config);
        match event {
            Event::Keyboard(KeyboardEvent::Key { key, state, .. }) => {
                // prevent double pressed / released keys
                if self.update_pressed_keys(handle, key, state) {
                    self.emulation.consume(event, handle).await?;
                }
                Ok(())
            }
            _ => self.emulation.consume(event, handle).await,
        }
    }

    pub async fn create(&mut self, handle: EmulationHandle) -> bool {
        if self.handles.insert(handle) {
            self.pressed_keys.insert(handle, HashSet::new());
            self.emulation.create(handle).await;
            true
        } else {
            false
        }
    }

    pub async fn destroy(&mut self, handle: EmulationHandle) {
        let _ = self.release_keys(handle).await;
        if self.handles.remove(&handle) {
            self.pressed_keys.remove(&handle);
            self.emulation.destroy(handle).await
        }
    }

    pub async fn terminate(&mut self) {
        for handle in self.handles.iter().cloned().collect::<Vec<_>>() {
            self.destroy(handle).await
        }
        self.emulation.terminate().await
    }

    pub async fn release_keys(&mut self, handle: EmulationHandle) -> Result<(), EmulationError> {
        if let Some(keys) = self.pressed_keys.get_mut(&handle) {
            let keys = keys.drain().collect::<Vec<_>>();
            for key in keys {
                let event = Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key,
                    state: 0,
                });
                self.emulation.consume(event, handle).await?;
                if let Ok(key) = input_event::scancode::Linux::try_from(key) {
                    log::warn!("releasing stuck key: {key:?}");
                }
            }
        }

        let event = Event::Keyboard(KeyboardEvent::Modifiers {
            depressed: 0,
            latched: 0,
            locked: 0,
            group: 0,
        });
        self.emulation.consume(event, handle).await?;
        Ok(())
    }

    pub fn has_pressed_keys(&self, handle: EmulationHandle) -> bool {
        self.pressed_keys
            .get(&handle)
            .is_some_and(|p| !p.is_empty())
    }

    pub fn update_config(&mut self, input_config: InputConfig) {
        self.input_config = input_config;
    }

    /// update the pressed_keys for the given handle
    /// returns whether the event should be processed
    fn update_pressed_keys(&mut self, handle: EmulationHandle, key: u32, state: u8) -> bool {
        let Some(pressed_keys) = self.pressed_keys.get_mut(&handle) else {
            return false;
        };

        if state == 0 {
            // currently pressed => can release
            pressed_keys.remove(&key)
        } else {
            // currently not pressed => can press
            pressed_keys.insert(key)
        }
    }
}

#[async_trait]
trait Emulation: Send {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError>;
    async fn create(&mut self, handle: EmulationHandle);
    async fn destroy(&mut self, handle: EmulationHandle);
    async fn terminate(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_processing_scales_pointer_motion() {
        let event = Event::Pointer(PointerEvent::Motion {
            time: 42,
            dx: 3.0,
            dy: -4.0,
        });
        let config = InputConfig {
            mouse_sensitivity: 1.5,
            ..Default::default()
        };

        assert_eq!(
            post_process_event(event, config),
            Event::Pointer(PointerEvent::Motion {
                time: 42,
                dx: 4.5,
                dy: -6.0,
            })
        );
    }

    #[test]
    fn post_processing_inverts_continuous_and_discrete_scrolling() {
        let config = InputConfig {
            invert_scroll: true,
            ..Default::default()
        };

        assert_eq!(
            post_process_event(
                Event::Pointer(PointerEvent::Axis {
                    time: 7,
                    axis: 0,
                    value: 2.5,
                }),
                config,
            ),
            Event::Pointer(PointerEvent::Axis {
                time: 7,
                axis: 0,
                value: -2.5,
            })
        );
        assert_eq!(
            post_process_event(
                Event::Pointer(PointerEvent::AxisDiscrete120 {
                    axis: 1,
                    value: 120,
                }),
                config,
            ),
            Event::Pointer(PointerEvent::AxisDiscrete120 {
                axis: 1,
                value: -120,
            })
        );
    }

    #[test]
    fn post_processing_leaves_other_events_unchanged() {
        let event = Event::Pointer(PointerEvent::Button {
            time: 5,
            button: 0x110,
            state: 1,
        });

        assert_eq!(
            post_process_event(
                event,
                InputConfig {
                    invert_scroll: true,
                    mouse_sensitivity: 2.0,
                },
            ),
            event
        );
    }
}
