use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Display,
    mem::swap,
    task::{Poll, ready},
};

use async_trait::async_trait;
use futures::StreamExt;
use futures_core::Stream;

use input_event::{Event, KeyboardEvent, PointerEvent, scancode};

pub use error::{CaptureCreationError, CaptureError, InputCaptureError};

pub mod error;

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
mod libei;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
mod layer_shell;

#[cfg(windows)]
mod windows;

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
mod x11;

/// fallback input capture (does not produce events)
mod dummy;

pub type CaptureHandle = u64;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum CaptureEvent {
    /// Capture on this handle is now active. `cursor`, when present,
    /// is the host's screen-space cursor position (in pixels) at the
    /// instant of the edge crossing — the capture loop forwards it to
    /// the peer as a [`ProtoEvent::MotionAbsolute`] so the guest's
    /// cursor lands at the visually-corresponding point on its own
    /// screen rather than snapping to the entry-edge midpoint.
    /// Backends that can't report cursor position emit `None` and
    /// the peer falls back to the entry-edge-midpoint warp.
    Begin { cursor: Option<(i32, i32)> },
    /// input event coming from capture handle
    Input(Event),
    /// the capture wrapper detected sustained back-toward-host motion
    /// past the configured threshold (the user has pinned the cursor
    /// at the host-adjacent edge of the guest and kept pushing). The
    /// capture loop should treat this like a release-bind chord.
    AutoRelease,
}

impl Display for CaptureEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureEvent::Begin { cursor: None } => write!(f, "begin capture"),
            CaptureEvent::Begin {
                cursor: Some((x, y)),
            } => write!(f, "begin capture @ ({x}, {y})"),
            CaptureEvent::Input(e) => write!(f, "{e}"),
            CaptureEvent::AutoRelease => write!(f, "auto-release"),
        }
    }
}

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
        write!(f, "{pos}")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    InputCapturePortal,
    #[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
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
            #[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
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

pub struct InputCapture {
    /// capture backend
    capture: Box<dyn Capture>,
    /// keys pressed by active capture
    pressed_keys: HashSet<scancode::Linux>,
    /// map from position to ids
    position_map: HashMap<Position, Vec<CaptureHandle>>,
    /// map from id to position
    id_map: HashMap<CaptureHandle, Position>,
    /// pending events
    pending: VecDeque<(CaptureHandle, CaptureEvent)>,
    /// pixel threshold for the cross-platform auto-release-on-wall-
    /// press fallback. 0 disables. See `track_wall_press`.
    release_threshold_px: u32,
    /// position the cursor is currently captured into, if any. Tracks
    /// `Begin`/release transitions so the wall-press accumulator
    /// resets correctly across capture sessions.
    capture_pos: Option<Position>,
    /// Modeled cursor position on the guest along the entry axis,
    /// relative to the host-adjacent edge. 0 = at the entry edge,
    /// growing values = further into the guest. Clamped at 0 from
    /// below; clamped at the cached peer extent from above when
    /// available, otherwise unbounded (degraded fallback).
    virtual_pos: f64,
    /// Pixels of back-toward-host motion that the modeled cursor
    /// could not absorb (proposed virtual_pos < 0). Resets whenever
    /// the cursor is back in the interior or moving deeper.
    wall_pressure: f64,
    /// Modeled guest cursor position in the guest's screen space,
    /// updated by accumulating Motion deltas while captured. Seeded
    /// on `Begin` from the cross-axis warp target (if peer bounds
    /// are known) or the entry-edge midpoint otherwise — i.e. wherever
    /// the guest's cursor visually lands at Enter. Read on release
    /// to compute a host-side warp so the local cursor reappears at
    /// the matching point on the host's screen instead of jumping
    /// back to where capture started.
    virtual_cursor: Option<(f64, f64)>,
    /// Per-position cache of peer display geometry. Populated when
    /// the peer responds with a `ProtoEvent::Bounds` event after
    /// Ack. Used as the upper clamp for `virtual_pos` so that
    /// pushing past the guest's actual far edge doesn't make the
    /// model run away. Only the entry-axis dimension is consulted.
    peer_bounds: HashMap<Position, (u32, u32)>,
}

/// Project a motion delta onto the entry axis. Positive return =
/// "into guest", so virtual_pos increases as the user pushes deeper.
fn entry_axis_delta(position: Position, dx: f64, dy: f64) -> f64 {
    match position {
        // Position::Left = guest is to the LEFT of host. User entered
        // by moving left (-dx). Convention: positive = into guest.
        Position::Left => -dx,
        Position::Right => dx,
        Position::Top => -dy,
        Position::Bottom => dy,
    }
}

impl InputCapture {
    /// create a new client with the given id
    pub async fn create(&mut self, id: CaptureHandle, pos: Position) -> Result<(), CaptureError> {
        assert!(!self.id_map.contains_key(&id));

        self.id_map.insert(id, pos);

        if let Some(v) = self.position_map.get_mut(&pos) {
            v.push(id);
            Ok(())
        } else {
            self.position_map.insert(pos, vec![id]);
            self.capture.create(pos).await
        }
    }

    /// destroy the client with the given id, if it exists
    pub async fn destroy(&mut self, id: CaptureHandle) -> Result<(), CaptureError> {
        let pos = self
            .id_map
            .remove(&id)
            .expect("no position for this handle");

        log::debug!("destroying capture {id} @ {pos}");
        let remaining = self.position_map.get_mut(&pos).expect("id vector");
        remaining.retain(|&i| i != id);

        log::debug!("remaining ids @ {pos}: {remaining:?}");
        if remaining.is_empty() {
            log::debug!("destroying capture @ {pos} - no remaining ids");
            self.position_map.remove(&pos);
            self.capture.destroy(pos).await?;
        }
        Ok(())
    }

    /// release mouse
    pub async fn release(&mut self) -> Result<(), CaptureError> {
        // Compute the host-side warp target before resetting the
        // wall-press / virtual_cursor state — once those are cleared
        // we lose the data needed to figure out where the guest's
        // cursor visually was.
        let warp_target = self
            .capture_pos
            .and_then(|pos| self.host_warp_target_on_release(pos));
        log::info!(
            "[release-warp] capture_pos={:?} virtual_cursor={:?} peer_bounds={:?} display_bounds={:?} → warp_target={warp_target:?}",
            self.capture_pos,
            self.virtual_cursor,
            self.capture_pos
                .and_then(|p| self.peer_bounds.get(&p).copied()),
            self.capture.display_bounds(),
        );
        self.pressed_keys.clear();
        self.reset_wall_press_state();
        self.capture.release(warp_target).await
    }

    /// Configure the wall-press auto-release pixel threshold.
    /// 0 disables. Effective immediately for the next motion event;
    /// no need to recreate the backend.
    pub fn set_release_threshold(&mut self, threshold: u32) {
        self.release_threshold_px = threshold;
    }

    /// Cache the peer's display geometry for a position. Used by
    /// the wall-press tracker as the upper bound for `virtual_pos`
    /// so the model can't run away when the user pushes past the
    /// peer's actual far edge.
    pub fn set_peer_bounds(&mut self, pos: Position, width: u32, height: u32) {
        log::debug!("peer at {pos} reports bounds {width}x{height}");
        self.peer_bounds.insert(pos, (width, height));
    }

    /// Forget the cached peer geometry for a position. Called when
    /// the corresponding capture is destroyed so re-adding the same
    /// peer later (potentially with new geometry) starts fresh.
    pub fn clear_peer_bounds(&mut self, pos: Position) {
        self.peer_bounds.remove(&pos);
    }

    /// Host's own display geometry — width and height in pixels of
    /// the union of all displays. Returns `None` when the active
    /// backend can't query its own bounds (e.g. xdg-desktop-portal,
    /// dummy). Used together with `peer_bounds` to compute a
    /// [`ProtoEvent::MotionAbsolute`] target so the guest cursor
    /// lands at the visually-corresponding point on Enter.
    pub fn display_bounds(&self) -> Option<(u32, u32)> {
        self.capture.display_bounds()
    }

    /// Host's screen-space cursor position normalized to the host's
    /// own display bounds (each axis in 0..1, clamped). Returns
    /// `None` when the active backend can't report its own bounds.
    /// Used for the self-sufficient `ProtoEvent::CursorPos` event
    /// (the receiver scales the normalized fraction against its
    /// own bounds and pins the entry axis to the matching edge), so
    /// the first crossing isn't blocked by the bootstrap problem
    /// `peer_warp_target` has — that variant requires a prior
    /// `Bounds` round-trip from the peer, which can't have happened
    /// yet on the very first Enter.
    pub fn host_normalized_cursor(&self, cursor: (i32, i32)) -> Option<(f32, f32)> {
        let (host_w, host_h) = self.display_bounds()?;
        if host_w == 0 || host_h == 0 {
            return None;
        }
        let (cx, cy) = cursor;
        let nx = (cx as f32 / host_w as f32).clamp(0.0, 1.0);
        let ny = (cy as f32 / host_h as f32).clamp(0.0, 1.0);
        Some((nx, ny))
    }

    /// Cursor warp target on the peer for a transition at `pos`,
    /// given the host's screen-space cursor position at the moment
    /// of crossing. Returns `None` when either the host's own
    /// `display_bounds` or the cached peer geometry is unavailable —
    /// in that case the capture loop just doesn't send MotionAbsolute
    /// and the guest falls back to its entry-edge-midpoint warp.
    ///
    /// Coordinates returned are pixels in the peer's screen space:
    /// the cross-axis is preserved as a normalized fraction of the
    /// host screen (so a host_y near the top maps to a peer_y near
    /// the top regardless of resolution mismatch), the on-axis is
    /// pinned to the peer's far edge for the entering side.
    pub fn peer_warp_target(&self, pos: Position, cursor: (i32, i32)) -> Option<(i32, i32)> {
        let (host_w, host_h) = self.display_bounds()?;
        let &(peer_w, peer_h) = self.peer_bounds.get(&pos)?;
        let (cx, cy) = cursor;
        let nx = (cx as f64 / host_w as f64).clamp(0.0, 1.0);
        let ny = (cy as f64 / host_h as f64).clamp(0.0, 1.0);
        let peer_w_i = peer_w as i32;
        let peer_h_i = peer_h as i32;
        let target = match pos {
            // Peer to our Left → cursor exits on left, enters peer on right
            Position::Left => (peer_w_i.saturating_sub(1), (ny * peer_h as f64) as i32),
            // Peer to our Right → cursor enters peer on left
            Position::Right => (0, (ny * peer_h as f64) as i32),
            // Peer above → cursor enters peer on bottom
            Position::Top => ((nx * peer_w as f64) as i32, peer_h_i.saturating_sub(1)),
            // Peer below → cursor enters peer on top
            Position::Bottom => ((nx * peer_w as f64) as i32, 0),
        };
        Some(target)
    }

    /// Returns the upper-clamp value (along the entry axis) for the
    /// given position, or `f64::INFINITY` if the peer hasn't reported
    /// bounds yet.
    fn peer_extent(&self, pos: Position) -> f64 {
        let Some(&(w, h)) = self.peer_bounds.get(&pos) else {
            return f64::INFINITY;
        };
        match pos {
            Position::Left | Position::Right => f64::from(w),
            Position::Top | Position::Bottom => f64::from(h),
        }
    }

    fn reset_wall_press_state(&mut self) {
        self.capture_pos = None;
        self.virtual_pos = 0.0;
        self.wall_pressure = 0.0;
        self.virtual_cursor = None;
    }

    /// Initial guest-space cursor position for a freshly-started
    /// capture. Mirrors what the guest's emulation will visibly do on
    /// the corresponding `Enter`: a `MotionAbsolute` warp target if
    /// the host can compute one (peer bounds known + capture backend
    /// reports cursor), otherwise the entry-edge midpoint that
    /// `entry_edge_for` produces on the guest side.
    fn initial_virtual_cursor(
        &self,
        pos: Position,
        host_cursor: Option<(i32, i32)>,
    ) -> Option<(f64, f64)> {
        if let Some(host_cursor) = host_cursor {
            if let Some((x, y)) = self.peer_warp_target(pos, host_cursor) {
                return Some((x as f64, y as f64));
            }
        }
        let &(peer_w, peer_h) = self.peer_bounds.get(&pos)?;
        let pw = peer_w as f64;
        let ph = peer_h as f64;
        Some(match pos {
            Position::Left => (0.0, ph / 2.0),
            Position::Right => ((pw - 1.0).max(0.0), ph / 2.0),
            Position::Top => (pw / 2.0, 0.0),
            Position::Bottom => (pw / 2.0, (ph - 1.0).max(0.0)),
        })
    }

    /// Where on the host's own screen the cursor should land when
    /// capture is released, given the modeled guest cursor position
    /// at the moment of release. Symmetric inverse of
    /// `peer_warp_target`: cross-axis is preserved as a normalized
    /// fraction of the peer's screen, on-axis is pinned to the
    /// host's far edge for the side the guest is on so the cursor
    /// reappears at the boundary it just crossed back through.
    fn host_warp_target_on_release(&self, pos: Position) -> Option<(i32, i32)> {
        let (gx, gy) = self.virtual_cursor?;
        let &(peer_w, peer_h) = self.peer_bounds.get(&pos)?;
        let (host_w, host_h) = self.capture.display_bounds()?;
        if peer_w == 0 || peer_h == 0 || host_w == 0 || host_h == 0 {
            return None;
        }
        let nx = (gx / peer_w as f64).clamp(0.0, 1.0);
        let ny = (gy / peer_h as f64).clamp(0.0, 1.0);
        let host_w_i = host_w as i32;
        let host_h_i = host_h as i32;
        Some(match pos {
            // Peer to our Left → cursor returns through host's left edge
            Position::Left => (0, (ny * host_h as f64) as i32),
            // Peer to our Right → cursor returns through host's right edge
            Position::Right => (host_w_i.saturating_sub(1), (ny * host_h as f64) as i32),
            // Peer above → cursor returns through host's top edge
            Position::Top => ((nx * host_w as f64) as i32, 0),
            // Peer below → cursor returns through host's bottom edge
            Position::Bottom => ((nx * host_w as f64) as i32, host_h_i.saturating_sub(1)),
        })
    }

    /// Update the wall-press accumulator from one event coming up
    /// from the backend. Returns true if the threshold was reached
    /// and an `AutoRelease` should be synthesized for the active
    /// capture position.
    fn track_wall_press(&mut self, pos: Position, event: &CaptureEvent) -> bool {
        match event {
            CaptureEvent::Begin { cursor } => {
                self.capture_pos = Some(pos);
                self.virtual_pos = 0.0;
                self.wall_pressure = 0.0;
                self.virtual_cursor = self.initial_virtual_cursor(pos, *cursor);
                false
            }
            CaptureEvent::AutoRelease => {
                // Don't reset virtual_cursor here — release() needs it
                // to compute the host-side warp target. The wrapper's
                // release() resets state after consuming it.
                false
            }
            CaptureEvent::Input(Event::Pointer(PointerEvent::Motion { dx, dy, .. })) => {
                let Some(active_pos) = self.capture_pos else {
                    return false;
                };
                if active_pos != pos {
                    return false;
                }

                // Track guest-space cursor for the on-release warp
                // back to the host. Clamped to the peer's bounds so
                // the model doesn't drift past the guest's screen
                // when the user pushes obliviously.
                if let (Some(vc), Some(&(peer_w, peer_h))) = (
                    self.virtual_cursor.as_mut(),
                    self.peer_bounds.get(&active_pos),
                ) {
                    vc.0 = (vc.0 + *dx).clamp(0.0, peer_w as f64);
                    vc.1 = (vc.1 + *dy).clamp(0.0, peer_h as f64);
                }

                if self.release_threshold_px == 0 {
                    return false;
                }

                let delta = entry_axis_delta(active_pos, *dx, *dy);
                let proposed = self.virtual_pos + delta;
                let upper = self.peer_extent(active_pos);
                // Clamp at 0 from below (host-adjacent edge — wall
                // pressure accumulates here) and at the peer's
                // entry-axis extent from above when known. The upper
                // clamp prevents the model from running away if the
                // user obliviously pushes their physical mouse past
                // the guest's actual far edge. When the peer hasn't
                // reported bounds yet (older peer, or pre-Ack
                // window), `upper` is INFINITY and we fall back to
                // the heuristic behavior.
                self.virtual_pos = proposed.clamp(0.0, upper);

                if proposed < 0.0 {
                    // Motion overshot the host-adjacent edge —
                    // accumulate the unabsorbed amount as wall
                    // pressure.
                    self.wall_pressure += -proposed;
                } else {
                    // Cursor moved into the interior or further in;
                    // reset so a brief bump against the wall followed
                    // by motion deeper into the guest doesn't combine
                    // with a later wall-press to fire spuriously.
                    self.wall_pressure = 0.0;
                }

                if self.wall_pressure >= f64::from(self.release_threshold_px) {
                    log::info!(
                        "auto-release: {:.0}px wall-press past entry edge ({}px threshold)",
                        self.wall_pressure,
                        self.release_threshold_px
                    );
                    self.reset_wall_press_state();
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Drain and return every key the capture has forwarded as
    /// down-but-not-up. The caller is expected to synthesize key-up
    /// events to the remote peer for each — otherwise the peer
    /// retains phantom-held keys after capture is released. The
    /// canonical case is the release-bind chord
    /// (Ctrl+Shift+Alt+Meta): the down events were sent while
    /// capture was active, but the matching up events arrive after
    /// the local tap has flipped to passthrough and never reach
    /// the peer.
    pub fn take_pressed_keys(&mut self) -> HashSet<scancode::Linux> {
        std::mem::take(&mut self.pressed_keys)
    }

    /// destroy the input capture
    pub async fn terminate(&mut self) -> Result<(), CaptureError> {
        self.capture.terminate().await
    }

    /// creates a new [`InputCapture`]
    pub async fn new(backend: Option<Backend>) -> Result<Self, CaptureCreationError> {
        let capture = create(backend).await?;
        Ok(Self {
            capture,
            id_map: Default::default(),
            pending: Default::default(),
            position_map: Default::default(),
            pressed_keys: HashSet::new(),
            release_threshold_px: 0,
            capture_pos: None,
            virtual_pos: 0.0,
            wall_pressure: 0.0,
            virtual_cursor: None,
            peer_bounds: HashMap::new(),
        })
    }

    /// check whether the given keys are pressed
    pub fn keys_pressed(&self, keys: &[scancode::Linux]) -> bool {
        keys.iter().all(|k| self.pressed_keys.contains(k))
    }

    fn update_pressed_keys(&mut self, key: u32, state: u8) {
        if let Ok(scancode) = scancode::Linux::try_from(key) {
            log::debug!("key: {key}, state: {state}, scancode: {scancode:?}");
            match state {
                1 => self.pressed_keys.insert(scancode),
                _ => self.pressed_keys.remove(&scancode),
            };
        }
    }
}

impl Stream for InputCapture {
    type Item = Result<(CaptureHandle, CaptureEvent), CaptureError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if let Some(e) = self.pending.pop_front() {
            return Poll::Ready(Some(Ok(e)));
        }

        // ready
        let event = ready!(self.capture.poll_next_unpin(cx));

        // stream closed
        let event = match event {
            Some(e) => e,
            None => return Poll::Ready(None),
        };

        // error occurred
        let (pos, event) = match event {
            Ok(e) => e,
            Err(e) => return Poll::Ready(Some(Err(e))),
        };

        // handle key presses
        if let CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key { key, state, .. })) = event {
            self.update_pressed_keys(key, state);
        }

        // wall-press auto-release tracking. Runs against every event
        // before routing so a single global accumulator stays consistent
        // regardless of how many handles exist at this position.
        let auto_release = self.track_wall_press(pos, &event);

        let len = self
            .position_map
            .get(&pos)
            .map(|ids| ids.len())
            .unwrap_or(0);

        match len {
            0 => Poll::Pending,
            1 => {
                let id = self.position_map.get(&pos).expect("no id")[0];
                if auto_release {
                    // Deliver the original motion first; queue the
                    // synthesized AutoRelease so the next poll picks
                    // it up.
                    self.pending.push_back((id, CaptureEvent::AutoRelease));
                }
                Poll::Ready(Some(Ok((id, event))))
            }
            _ => {
                let mut position_map = HashMap::new();
                swap(&mut self.position_map, &mut position_map);
                {
                    for &id in position_map.get(&pos).expect("position") {
                        self.pending.push_back((id, event));
                        if auto_release {
                            self.pending.push_back((id, CaptureEvent::AutoRelease));
                        }
                    }
                }
                swap(&mut self.position_map, &mut position_map);

                Poll::Ready(Some(Ok(self.pending.pop_front().expect("event"))))
            }
        }
    }
}

#[async_trait]
trait Capture: Stream<Item = Result<(Position, CaptureEvent), CaptureError>> + Unpin {
    /// create a new client with the given id
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError>;

    /// destroy the client with the given id, if it exists
    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError>;

    /// release mouse. `warp_target`, when present, is a screen-space
    /// pixel point on the host's own display where the local cursor
    /// should be placed before becoming visible again — used to
    /// preserve cross-axis continuity when capture ends so the cursor
    /// reappears next to where it visually was on the guest, not at
    /// the spot where capture started. Backends that don't hide the
    /// system cursor or can't warp it can ignore the parameter.
    async fn release(&mut self, warp_target: Option<(i32, i32)>) -> Result<(), CaptureError>;

    /// destroy the input capture
    async fn terminate(&mut self) -> Result<(), CaptureError>;

    /// Host's own display geometry. Default implementation returns
    /// `None`; backends that can query their own dimensions override
    /// (currently macOS via CGDisplay; others may add this later).
    fn display_bounds(&self) -> Option<(u32, u32)> {
        None
    }
}

async fn create_backend(
    backend: Backend,
) -> Result<
    Box<dyn Capture<Item = Result<(Position, CaptureEvent), CaptureError>>>,
    CaptureCreationError,
> {
    match backend {
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        Backend::InputCapturePortal => Ok(Box::new(libei::LibeiInputCapture::new().await?)),
        #[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
        Backend::LayerShell => Ok(Box::new(layer_shell::LayerShellInputCapture::new()?)),
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        Backend::X11 => Ok(Box::new(x11::X11InputCapture::new()?)),
        #[cfg(windows)]
        Backend::Windows => Ok(Box::new(windows::WindowsInputCapture::new())),
        #[cfg(target_os = "macos")]
        Backend::MacOs => Ok(Box::new(macos::MacOSInputCapture::new().await?)),
        Backend::Dummy => Ok(Box::new(dummy::DummyInputCapture::new())),
    }
}

async fn create(
    backend: Option<Backend>,
) -> Result<
    Box<dyn Capture<Item = Result<(Position, CaptureEvent), CaptureError>>>,
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
        #[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
        Backend::LayerShell,
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        Backend::X11,
        #[cfg(windows)]
        Backend::Windows,
        #[cfg(target_os = "macos")]
        Backend::MacOs,
    ] {
        match create_backend(backend).await {
            Ok(b) => {
                log::info!("using capture backend: {backend}");
                return Ok(b);
            }
            Err(e) if e.cancelled_by_user() => return Err(e),
            Err(e) => log::warn!("{backend} input capture backend unavailable: {e}"),
        }
    }
    Err(CaptureCreationError::NoAvailableBackend)
}
