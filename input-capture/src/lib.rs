use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Display,
    future::Future,
    mem::swap,
    pin::Pin,
    task::{Poll, ready},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::StreamExt;
use futures_core::Stream;
use tokio::time::Sleep;

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
    /// instant of the edge crossing — the capture loop normalizes it
    /// against the host's display bounds and forwards it to the peer
    /// as a [`ProtoEvent::CursorPos`] so the guest's cursor lands at
    /// the visually-corresponding point on its own screen. Backends
    /// that can't report cursor position emit `None`; the peer's
    /// cursor stays where it was on remote-takeover (no forced
    /// midpoint warp — that masquerades as a mid-screen crossing on
    /// fast re-crosses).
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
    /// Host-coord cursor at the moment of `Begin`, retained until
    /// `peer_bounds` arrives so we can retroactively seed
    /// `virtual_cursor` once the round-trip completes. Without this,
    /// a `Begin` that fires before the peer's `Bounds` reply leaves
    /// `virtual_cursor` stuck at `None` for the rest of the session
    /// — the wall-press accumulator skips updates and the
    /// release-time warp falls back to the original crossing
    /// y-value instead of where the cursor visually was on the peer.
    pending_begin_cursor: Option<(i32, i32)>,
    /// Motion deltas that arrived while `virtual_cursor` was still
    /// `None` (between `Begin` and the late-arriving
    /// `set_peer_bounds`). Drained into the freshly-seeded
    /// `virtual_cursor` when the bootstrap completes so deltas
    /// during the round-trip aren't lost.
    pending_motion: (f64, f64),
    /// Per-position cache of peer display geometry. Populated when
    /// the peer responds with a `ProtoEvent::Bounds` event after
    /// Ack. Used as the upper clamp for `virtual_pos` so that
    /// pushing past the guest's actual far edge doesn't make the
    /// model run away. Only the entry-axis dimension is consulted.
    peer_bounds: HashMap<Position, (u32, u32)>,
    /// Set when wall_pressure first crosses `release_threshold_px`,
    /// cleared when the peer's handover Leave arrives (which routes
    /// through `release_no_host_warp` → `reset_wall_press_state`) or
    /// when the cursor moves back into the interior. The wall-press
    /// auto-release fires only after `wall_press_deadline` elapses
    /// without being cleared — turning the historically race-y
    /// "wall-press vs peer-Leave" into an explicit fallback that
    /// only kicks in when the peer can't deliver a Leave (lock
    /// screen, restricted DE, dead peer).
    wall_press_pending_at: Option<Instant>,
    /// Window after the threshold is crossed during which a peer
    /// Leave can cancel the deferred AutoRelease. Sized so a
    /// healthy LAN round-trip beats it comfortably.
    wall_press_deadline: Duration,
    /// Timer driving the deferred fire. Reset to deadline-from-now
    /// on first threshold crossing; polled in `poll_next` so the
    /// fire happens even when no further backend events arrive
    /// (the user pinned the cursor against the wall and stopped).
    wall_press_timer: Pin<Box<Sleep>>,
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

    /// Release without applying a host-side cursor warp. Used when
    /// the remote peer is taking over (it just sent us Enter +
    /// CursorPos): the proportional warp from CursorPos is the
    /// authoritative final position for our shared cursor, and the
    /// stale `virtual_cursor`-derived warp would race against it
    /// and frequently win — clobbering the proportional landing
    /// with whatever position Linux *thought* the peer's cursor was
    /// at before the user moved it.
    pub async fn release_no_host_warp(&mut self) -> Result<(), CaptureError> {
        log::info!(
            "[release-warp] handover release: capture_pos={:?} — skipping host warp, peer's CursorPos is authoritative",
            self.capture_pos,
        );
        self.pressed_keys.clear();
        self.reset_wall_press_state();
        self.capture.release(None).await
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
    ///
    /// If `Begin` fired before this arrived (the round-trip
    /// bootstrap case — `Bounds` is sent in response to `Enter`,
    /// which is sent by the host AFTER `Begin` fires), seed
    /// `virtual_cursor` retroactively so the wall-press / release
    /// machinery has a baseline to track from. Drains any motion
    /// that piled up in `pending_motion` so deltas during the
    /// round-trip aren't lost.
    pub fn set_peer_bounds(&mut self, pos: Position, width: u32, height: u32) {
        log::debug!("peer at {pos} reports bounds {width}x{height}");
        self.peer_bounds.insert(pos, (width, height));

        if self.virtual_cursor.is_none()
            && self.capture_pos == Some(pos)
            && self.pending_begin_cursor.is_some()
        {
            let begin_cursor = self.pending_begin_cursor;
            let seeded = self.initial_virtual_cursor(pos, begin_cursor);
            if let Some((sx, sy)) = seeded {
                let (mx, my) = self.pending_motion;
                let peer_w = width as f64;
                let peer_h = height as f64;
                self.virtual_cursor =
                    Some(((sx + mx).clamp(0.0, peer_w), (sy + my).clamp(0.0, peer_h)));
                self.pending_motion = (0.0, 0.0);
                log::info!(
                    "[bootstrap] seeded virtual_cursor={:?} after late peer_bounds at {pos} (drained pending_motion=({mx:.1}, {my:.1}))",
                    self.virtual_cursor
                );
            }
        }
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
    /// dummy). Used by `host_normalized_cursor` to compute the
    /// [`ProtoEvent::CursorPos`] fraction the guest scales against
    /// its own bounds on Enter.
    pub fn display_bounds(&self) -> Option<(u32, u32)> {
        self.capture.display_bounds()
    }

    /// Top-left corner of the host's display union in pointer-event
    /// coordinate space. See `Capture::display_origin` for why this
    /// matters on multi-monitor macOS hosts.
    fn display_origin(&self) -> (i32, i32) {
        self.capture.display_origin()
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
        let (origin_x, origin_y) = self.display_origin();
        let (cx, cy) = cursor;
        // Subtract the union origin before normalizing so that
        // points on a non-origin display (e.g. a macOS external
        // monitor positioned to the left of the primary, where
        // cursor x is negative) map correctly. Without this, the
        // clamp masks every off-primary point as the screen edge.
        let nx = ((cx - origin_x) as f32 / host_w as f32).clamp(0.0, 1.0);
        let ny = ((cy - origin_y) as f32 / host_h as f32).clamp(0.0, 1.0);
        Some((nx, ny))
    }

    /// Cursor warp target on the peer for a transition at `pos`,
    /// given the host's screen-space cursor position at the moment
    /// of crossing. Returns `None` when either the host's own
    /// `display_bounds` or the cached peer geometry is unavailable —
    /// in that case there's no warp target to compute and the peer's
    /// cursor stays wherever the most recent `CursorPos` (or, if none
    /// arrived this session, where it was) put it.
    ///
    /// Coordinates returned are pixels in the peer's screen space:
    /// the cross-axis is preserved as a normalized fraction of the
    /// host screen (so a host_y near the top maps to a peer_y near
    /// the top regardless of resolution mismatch), the on-axis is
    /// pinned to the peer's far edge for the entering side.
    pub fn peer_warp_target(&self, pos: Position, cursor: (i32, i32)) -> Option<(i32, i32)> {
        let (host_w, host_h) = self.display_bounds()?;
        let &(peer_w, peer_h) = self.peer_bounds.get(&pos)?;
        let (origin_x, origin_y) = self.display_origin();
        let (cx, cy) = cursor;
        // Subtract the union origin before normalizing — same
        // rationale as in host_normalized_cursor.
        let nx = ((cx - origin_x) as f64 / host_w as f64).clamp(0.0, 1.0);
        let ny = ((cy - origin_y) as f64 / host_h as f64).clamp(0.0, 1.0);
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
        self.pending_begin_cursor = None;
        self.pending_motion = (0.0, 0.0);
        // Cancel any deferred AutoRelease — release() / handover have
        // taken responsibility for the transition.
        self.wall_press_pending_at = None;
    }

    /// Initial guest-space cursor position for a freshly-started
    /// capture. Mirrors what the guest's emulation will visibly do on
    /// the corresponding `Enter`: the `CursorPos` proportional warp
    /// target if the host can compute one (capture backend reports
    /// cursor), otherwise the entry-edge midpoint as a fallback for
    /// the wall-press model's starting position.
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
        let (origin_x, origin_y) = self.display_origin();
        let nx = (gx / peer_w as f64).clamp(0.0, 1.0);
        let ny = (gy / peer_h as f64).clamp(0.0, 1.0);
        let host_w_i = host_w as i32;
        let host_h_i = host_h as i32;
        // Add the union origin back so the result is in pointer-event
        // coordinate space (which is what `CGDisplay::warp_mouse_cursor_position`
        // and friends consume), not "0..host_w" of the union rectangle.
        // Matters on macOS hosts whose primary isn't anchored at (0, 0)
        // — `display_bounds` returns just the size of the union, so the
        // origin needs to be reapplied to recover absolute coords.
        Some(match pos {
            // Peer to our Left → cursor returns through host's left edge
            Position::Left => (origin_x, origin_y + (ny * host_h as f64) as i32),
            // Peer to our Right → cursor returns through host's right edge
            Position::Right => (
                origin_x + host_w_i.saturating_sub(1),
                origin_y + (ny * host_h as f64) as i32,
            ),
            // Peer above → cursor returns through host's top edge
            Position::Top => (origin_x + (nx * host_w as f64) as i32, origin_y),
            // Peer below → cursor returns through host's bottom edge
            Position::Bottom => (
                origin_x + (nx * host_w as f64) as i32,
                origin_y + host_h_i.saturating_sub(1),
            ),
        })
    }

    /// Update the wall-press accumulator from one event coming up
    /// from the backend. Sets `wall_press_pending_at` (and arms the
    /// timer) when the threshold is first crossed; the actual
    /// `AutoRelease` synthesis happens in `poll_next` once the
    /// deadline elapses without a peer Leave clearing the pending
    /// flag.
    fn track_wall_press(&mut self, pos: Position, event: &CaptureEvent) {
        match event {
            CaptureEvent::Begin { cursor } => {
                self.capture_pos = Some(pos);
                self.virtual_pos = 0.0;
                self.wall_pressure = 0.0;
                self.virtual_cursor = self.initial_virtual_cursor(pos, *cursor);
                // Stash the host-coord cursor so set_peer_bounds can
                // retroactively seed virtual_cursor if peer_bounds
                // arrives after Begin.
                self.pending_begin_cursor = *cursor;
                self.pending_motion = (0.0, 0.0);
                log::info!(
                    "[wp-begin] pos={pos} cursor={cursor:?} peer_bounds={:?} virtual_cursor={:?}",
                    self.peer_bounds.get(&pos).copied(),
                    self.virtual_cursor,
                );
            }
            CaptureEvent::AutoRelease => {
                // Don't reset virtual_cursor here — release() needs it
                // to compute the host-side warp target. The wrapper's
                // release() resets state after consuming it.
            }
            CaptureEvent::Input(Event::Pointer(PointerEvent::Motion { dx, dy, .. })) => {
                let Some(active_pos) = self.capture_pos else {
                    return;
                };
                if active_pos != pos {
                    return;
                }

                // Track guest-space cursor for the on-release warp
                // back to the host. Clamped to the peer's bounds so
                // the model doesn't drift past the guest's screen
                // when the user pushes obliviously.
                match (
                    self.virtual_cursor.as_mut(),
                    self.peer_bounds.get(&active_pos),
                ) {
                    (Some(vc), Some(&(peer_w, peer_h))) => {
                        vc.0 = (vc.0 + *dx).clamp(0.0, peer_w as f64);
                        vc.1 = (vc.1 + *dy).clamp(0.0, peer_h as f64);
                    }
                    // virtual_cursor not yet seeded (peer_bounds was
                    // None at Begin time and the round-trip hasn't
                    // completed yet). Buffer the deltas so they can
                    // be applied retroactively in set_peer_bounds
                    // once the bootstrap finishes — otherwise the
                    // motion that happened during the round-trip is
                    // silently lost and the release-time warp picks
                    // the wrong y.
                    (None, _) => {
                        self.pending_motion.0 += *dx;
                        self.pending_motion.1 += *dy;
                        log::debug!(
                            "[wp-motion] deferred dx={dx:.1} dy={dy:.1} (peer_bounds for {active_pos}: {:?})",
                            self.peer_bounds.get(&active_pos).copied(),
                        );
                    }
                    _ => {}
                }

                if self.release_threshold_px == 0 {
                    return;
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
                    if self.wall_press_pending_at.take().is_some() {
                        log::info!(
                            "wall-press deferred AutoRelease cancelled (cursor moved away from entry edge)"
                        );
                    }
                }

                if self.wall_pressure >= f64::from(self.release_threshold_px)
                    && self.wall_press_pending_at.is_none()
                {
                    let now = Instant::now();
                    self.wall_press_pending_at = Some(now);
                    self.wall_press_timer
                        .as_mut()
                        .reset(tokio::time::Instant::from_std(
                            now + self.wall_press_deadline,
                        ));
                    log::info!(
                        "wall-press threshold reached ({:.0}px past entry edge, {}px threshold) — \
                         deferring AutoRelease for {}ms pending peer Leave",
                        self.wall_pressure,
                        self.release_threshold_px,
                        self.wall_press_deadline.as_millis(),
                    );
                }
                // Fire is now driven by the timer in `poll_next`, not
                // directly from this event — keeps the behavior gated
                // on "peer didn't claim handover in time" instead of
                // racing the peer's Leave.
            }
            _ => {}
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
            pending_begin_cursor: None,
            pending_motion: (0.0, 0.0),
            peer_bounds: HashMap::new(),
            wall_press_pending_at: None,
            wall_press_deadline: Duration::from_millis(150),
            wall_press_timer: Box::pin(tokio::time::sleep(Duration::from_secs(0))),
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

        // Deferred wall-press fallback. If the threshold was crossed
        // and the deadline elapsed without a peer Leave clearing
        // `wall_press_pending_at` (release_no_host_warp →
        // reset_wall_press_state), synthesize AutoRelease for every
        // capture handle at the active position. Polled before the
        // backend so a fire still happens when the user pinned the
        // cursor against the wall and stopped moving (no further
        // backend events, but the deadline still has to elapse).
        if self.wall_press_pending_at.is_some()
            && self.wall_press_timer.as_mut().poll(cx).is_ready()
        {
            self.wall_press_pending_at = None;
            log::info!(
                "wall-press deadline elapsed ({}ms) — firing AutoRelease (no peer Leave; \
                 assuming peer-side capture is unavailable, e.g. lock screen)",
                self.wall_press_deadline.as_millis(),
            );
            if let Some(pos) = self.capture_pos {
                if let Some(ids) = self.position_map.get(&pos).cloned() {
                    for id in ids {
                        self.pending.push_back((id, CaptureEvent::AutoRelease));
                    }
                }
            }
            if let Some(e) = self.pending.pop_front() {
                return Poll::Ready(Some(Ok(e)));
            }
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
        // regardless of how many handles exist at this position. The
        // fire itself is deferred and driven by `wall_press_timer`
        // above so the peer's Leave can cancel it.
        self.track_wall_press(pos, &event);

        let len = self
            .position_map
            .get(&pos)
            .map(|ids| ids.len())
            .unwrap_or(0);

        match len {
            0 => Poll::Pending,
            1 => {
                let id = self.position_map.get(&pos).expect("no id")[0];
                Poll::Ready(Some(Ok((id, event))))
            }
            _ => {
                let mut position_map = HashMap::new();
                swap(&mut self.position_map, &mut position_map);
                {
                    for &id in position_map.get(&pos).expect("position") {
                        self.pending.push_back((id, event));
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

    /// Top-left corner of the union of all displays in the host's
    /// global pointer-coordinate system. Defaults to (0, 0) — fine
    /// for any backend whose primary display is the origin (Windows,
    /// most X11/Wayland setups). Returns the actual `(xmin, ymin)`
    /// on macOS, where the global coordinate system is anchored at
    /// the primary's top-left and a left-attached external display
    /// occupies negative x. Used by `host_normalized_cursor` and
    /// `peer_warp_target` to correctly normalize cursor positions
    /// outside the primary display — without this, the
    /// `clamp(0.0, 1.0)` in those helpers silently maps every point
    /// on a non-origin display to the screen edge.
    fn display_origin(&self) -> (i32, i32) {
        (0, 0)
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
