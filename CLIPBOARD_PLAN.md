# Per-pair clipboard sync + app-suppression — implementation plan

Self-contained working plan so a fresh context can pick this up and execute. All
design decisions below are user-signed-off; open questions section at the bottom
is intentionally empty.

## Context

PR #327 (Daniel Nakov) added bidirectional clipboard sharing across all
connected peers, gated by a single global `enable_clipboard` boolean in
`config.toml`. We want the same feature, but:

1. **Per-pair gating** instead of global — `clipboard_send` on each
   outgoing `ClientConfig`, `clipboard_receive` on each incoming
   `IncomingPeerConfig`. Sync only happens when both ends have their
   respective bit on. Aligns with the per-pair architecture we already
   shipped for scroll & sensitivity in PR #435 (`IncomingPeerConfig`,
   `AdwExpanderRow` per peer).
2. **App-source suppression** — user-maintained list of apps whose
   clipboard contents must never be sent to peers. Specifically password
   managers (1Password, Bitwarden, LastPass, KeePassXC, etc.). Plus
   automatic suppression of macOS clipboard items marked with
   `org.nspasteboard.ConcealedType` UTI.

## Decisions (signed off)

1. **Defaults:** both `clipboard_send` and `clipboard_receive` default to
   **`false`** (opt-in). Clipboard contents are a meaningfully different
   trust scope than mouse/keyboard; authorization-for-input shouldn't
   imply authorization-for-clipboard.
2. **macOS concealed-type auto-suppression:** **include in Phase 4**.
   Catches password managers without the user maintaining a list. Layer
   the user-maintained list as a supplement.
3. **Linux compositor coverage:** **Hyprland + Sway + X11 only** for
   v1. KDE / GNOME require DBus calls / Shell extensions and can be a
   follow-up. Users on those compositors won't have suppression
   detection (we surface that as a known limitation in the README of
   the new PR).
4. **App-suppression modal UX:** **running-apps tab + manual-entry tab
   only**. No "recently active during copies" LRU for v1.

## What's reusable from #327

Cherry-pickable as-is, with `Co-Authored-By: Daniel Nakov <…>` trailer:

- `arboard` crate dependency (cross-platform clipboard primitive).
- `input-capture/src/clipboard.rs` (`ClipboardMonitor`: 500 ms poll,
  200 ms debounce, `update_last_content` to prevent reading our own
  writes as external changes).
- `input-emulation/src/clipboard.rs` (`ClipboardEmulation::set()`,
  blocking-task wrapper).
- `Event::Clipboard(ClipboardEvent::Text(String))` enum variant.
- Per-backend emulation wiring (libei, macos, windows, wlroots, xdp) —
  small additions consuming `ClipboardEvent`.

What we **rewrite**:

- The global `enable_clipboard` flag → per-pair bits.
- The capture-side broadcast logic → per-peer fan-out by `clipboard_send`.
- The listen-side accept logic → per-peer gate by `clipboard_receive`.
- `ProtoEvent::Clipboard` gets an originator-fingerprint field for
  N-peer loop prevention (#327's 200 ms debounce only solves N=2).

## Branch strategy

- **Base:** branch off `split/08-scroll` because we depend on the
  `IncomingPeerConfig` schema introduced there.
- **Branch name:** `feat/clipboard-per-pair`.
- **PR target:** `main`. Note in the description: "depends on #435
  landing first; until then the diff includes #435's commits as
  context. Will rebase onto `main` once #435 merges."
- After #435 merges to upstream `main`, rebase
  `feat/clipboard-per-pair` onto `main` so the PR's diff shrinks to
  just the clipboard work.
- Alternative if #435 is already merged when we start: branch off
  `main` directly.

## Phase 1 — lift the reusable bits from #327

Single commit. Behavior change: zero (primitives only, no service wiring).

**Commit message:** `feat(clipboard): vendor primitives + protocol from #327`
with `Co-Authored-By: Daniel Nakov <NN@…>` (resolve email via
`git -C /home/jon/Code/lan-mouse log --pretty='%an <%ae>' …` against
the source commit on `feschber/lan-mouse#327`).

**Files:**

- `Cargo.toml` (workspace) and `input-capture/Cargo.toml` /
  `input-emulation/Cargo.toml` — add `arboard` dep at the version
  used in #327.
- **NEW** `input-capture/src/clipboard.rs` — lift verbatim.
- **NEW** `input-emulation/src/clipboard.rs` — lift verbatim.
- `input-event/src/lib.rs` — add `ClipboardEvent::Text(String)` and
  the `Event::Clipboard(ClipboardEvent)` variant.
- `lan-mouse-proto/src/lib.rs` — add `ProtoEvent::Clipboard {
  from_fingerprint: String, content: String }`. (Note the
  pre-baked `from_fingerprint` field — not in #327. See Phase 2 loop
  prevention.) `EventType::Clipboard` + encode/decode (length-prefixed
  strings; mind `MAX_EVENT_SIZE` so total fits — content cap stays at
  ~4 KB minus fingerprint overhead).
- Per-backend trait method addition on `Emulation`:
  `async fn set_clipboard(&self, text: String) -> Result<…>` with a
  default no-op. Concrete impls in macos.rs / wlroots.rs /
  libei.rs / windows.rs / xdg_desktop_portal.rs delegate to
  `ClipboardEmulation::set()`.
- `Cargo.lock` churn accepted once.

**Build check:** `cargo build --release -p lan-mouse` should be clean.
No new behavior triggers.

## Phase 2 — per-pair config + IPC + Service routing

The architectural commit. Behavior change: clipboard sync starts
working, gated per-pair.

**Files:**

- `lan-mouse-ipc/src/lib.rs`:
  - `IncomingPeerConfig`: add `clipboard_receive: bool` (default
    `false`). Update the custom `Deserialize` to default-false on
    legacy entries.
  - `ClientConfig`: add `clipboard_send: bool` (default `false`).
    Update its serde derives.
  - New requests: `FrontendRequest::SetClientClipboardSend(ClientHandle, bool)`,
    `FrontendRequest::SetIncomingPeerClipboardReceive(String, bool)`.
  - `FrontendEvent`: rely on existing `State` + `AuthorizedUpdated`
    broadcasts to push values back to GUI; no new event variant.
- `src/config.rs`: legacy-friendly load (already handled via
  `IncomingPeerConfig`'s untagged-enum `Deserialize`). Add migration
  for `ClientConfig` so old `clients = […]` entries default the new
  field to false.
- `src/service.rs`:
  - Handlers for the two new requests; mutate `client_manager` /
    `authorized_keys`, broadcast updated state, save config.
  - **Capture-side broadcast site:** when `ClipboardMonitor` emits
    a `Clipboard` event, fan out via `LanMouseConnection`, but only
    to peers where the `ClientConfig.clipboard_send` is true. Stamp
    the originator fingerprint (this device's
    `public_key_fingerprint`) onto the `ProtoEvent::Clipboard`
    before send.
  - **Listen-side accept site:** when `ProtoEvent::Clipboard`
    arrives, look up the peer's `IncomingPeerConfig` by fingerprint
    via existing `addr_to_fingerprint` cache. Drop with debug log
    if `clipboard_receive` is false. Otherwise:
    1. Inject locally via `ClipboardEmulation::set`.
    2. Loop-prevention check (see below).
    3. Forward onward to other peers whose `clipboard_send` is true,
       skipping the originator and any peer fingerprint we've already
       forwarded the same content to within the last 1 s.
- `src/capture.rs` — wire up `ClipboardMonitor` and route its events
  through Service. Spawn the monitor at Service start.
- `src/connect.rs` / `src/listen.rs` — pass through the new ProtoEvent.

**Loop prevention (N-peer):**

- `Service` keeps a small in-memory map
  `recent_forwarded: HashMap<(String /*from_fp*/, u64 /*content_hash*/), Instant>`
  with a 1 s eviction sweep.
- Before forwarding, check the (origin, hash) tuple. If recently
  forwarded, skip. If not, record and proceed.
- The originator fingerprint guarantees A → B → C still terminates
  cleanly even if C is also subscribed to A directly.

**Build + smoke test:** with two peers, both with their respective
toggles on, copy on A and verify clipboard on B updates. Toggle B's
`clipboard_receive` off and verify content stops landing.

## Phase 3 — per-pair GTK toggles

Two new switch rows mirroring what we shipped in #435.

**Files:**

- `lan-mouse-gtk/resources/client_row.ui` — new
  `AdwSwitchRow` titled "Share clipboard with this peer", subtitle
  "Allow this peer to receive copies you make on this device.".
  `activatable=false` to not collapse the expander on click.
- `lan-mouse-gtk/src/client_row/imp.rs` — template_child + signal
  wiring; emits `request-clipboard-send-change(bool)`.
- `lan-mouse-gtk/src/client_row.rs` — refresh helper, property-notify
  on `ClientObject` for `clipboard-send`.
- `lan-mouse-gtk/src/client_object*.rs` — new property
  `clipboard-send` (bool).
- `lan-mouse-gtk/resources/key_row.ui` — new
  `AdwSwitchRow` titled "Accept clipboard from this peer".
- `lan-mouse-gtk/src/key_row/imp.rs` — template_child + signal,
  emits `request-clipboard-receive-change(bool)`.
- `lan-mouse-gtk/src/key_row.rs` — refresh helper, property-notify
  on `KeyObject`.
- `lan-mouse-gtk/src/key_object*.rs` — new property
  `clipboard-receive` (bool).
- `lan-mouse-gtk/src/window.rs` — new signal closures dispatch to
  `FrontendRequest::SetClientClipboardSend` /
  `SetIncomingPeerClipboardReceive`.
- Optionally extend `format_summary_parts` in `key_row.rs` to
  surface clipboard state in the collapsed-row summary.

## Phase 4 — app-suppression infrastructure

The biggest chunk of new code. Cross-platform "what's the frontmost
app" abstraction + concealed-type detection on macOS + the suppression
list itself.

**New crate or new module?** Lives in `input-capture` since it's
read by `ClipboardMonitor`. New file:
`input-capture/src/frontmost_app.rs`.

**Files:**

- **NEW** `input-capture/src/frontmost_app.rs`:

  ```rust
  pub enum AppIdent {
      MacBundle(String),       // e.g. "com.1password.1password7"
      WindowsExe(String),      // e.g. "1Password.exe" (basename, lowercased)
      LinuxX11(String),        // WM_CLASS instance/name
      LinuxWayland(String),    // xdg-toplevel app_id
  }

  pub fn frontmost_app() -> Option<AppIdent>;
  pub fn list_running_apps() -> Vec<AppIdent>;  // for the "From running apps" tab
  ```

  Per-platform impl via `cfg`:
  - **macOS:** `objc2` to call
    `NSWorkspace.frontmostApplication.bundleIdentifier`. List via
    `NSWorkspace.runningApplications`.
  - **Windows:** `GetForegroundWindow()` →
    `GetWindowThreadProcessId()` → `OpenProcess` +
    `QueryFullProcessImageNameW` → basename. List via
    `EnumProcesses` + `QueryFullProcessImageNameW`.
  - **Linux/Wayland:** shell out to `hyprctl activewindow -j` and
    `swaymsg -t get_tree`, parse JSON `app_id`. Fallback `None`
    on KDE / GNOME / unknown compositors. List via the same IPC
    queries iterating the tree.
  - **Linux/X11:** `xcb` for `_NET_ACTIVE_WINDOW` →
    `WM_CLASS`. List via `_NET_CLIENT_LIST`.

- `input-capture/src/clipboard.rs` (modify the lifted file):
  - Take an `Arc<Mutex<SuppressionList>>` constructor arg.
  - In the change-detection loop, before emitting, call
    `frontmost_app()` and check membership.
  - On macOS, also call a new `is_concealed_clipboard()` helper
    that checks `NSPasteboard.types` for `org.nspasteboard.ConcealedType`.
    (Layer this on top of `arboard`'s text read; one extra
    `objc2` call.)
  - Suppressed path: log at debug, **do NOT** call
    `update_last_content`. (Reasoning: if user pastes a password
    then copies a non-secret then copies the same password again,
    we still want to sync the non-secret correctly. Forgetting
    "we saw this content" lets later copies through.)

- `lan-mouse-ipc/src/lib.rs`:
  - New `pub struct ClipboardSuppression { pub apps: Vec<AppIdent> }`
    on `Config` (or just `Vec<AppIdent>` directly).
  - New requests: `FrontendRequest::AddSuppressedApp(AppIdent)`,
    `FrontendRequest::RemoveSuppressedApp(AppIdent)`,
    `FrontendRequest::ListRunningApps`.
  - Event: `FrontendEvent::SuppressedAppsUpdated(Vec<AppIdent>)`,
    `FrontendEvent::RunningApps(Vec<AppIdent>)`.

- `src/config.rs`: persist `clipboard_suppress_apps` at the top
  level of `config.toml`.

- `src/service.rs`: handlers for the new requests. Push the
  `Arc<Mutex<SuppressionList>>` into the spawned `ClipboardMonitor`
  at startup; refresh on `AddSuppressedApp` / `RemoveSuppressedApp`.

## Phase 5 — app-suppression GTK surface

UI for managing the suppression list. Modal dialog mirrors
`AuthorizationWindow`.

**Files:**

- `lan-mouse-gtk/resources/window.ui` — new "Clipboard Privacy"
  preferences group near the bottom (after "Network Discovery").
  Single `AdwActionRow` with title "Suppressed apps", subtitle
  showing count ("0 apps" / "1 app" / "N apps"), suffix button
  "Manage" that opens the modal.

- **NEW** `lan-mouse-gtk/resources/clipboard_privacy_window.ui`:
  modal `AdwWindow` with:
  - Title "Apps that won't share their clipboard"
  - Header bar with close button
  - `AdwPreferencesGroup` listing current entries; each entry is
    a row with the app identifier and a delete button
  - `AdwPreferencesGroup` with "Add app" `AdwActionRow` whose
    suffix button opens a sub-dialog
  - Empty-state placeholder when list is empty

- **NEW** `lan-mouse-gtk/resources/add_suppressed_app_window.ui`:
  sub-modal with two-tab `AdwViewStack`:
  - **From running apps** — `GtkListView` populated from
    `FrontendEvent::RunningApps`. Refreshed when the dialog opens.
  - **Manual entry** — `GtkEntry` for free-form text + dropdown
    for platform/identifier-type (defaults to current platform's
    natural identifier). Validation hint: "On macOS use the bundle
    ID (e.g. `com.1password.1password7`); on Windows use the
    executable name (e.g. `1Password.exe`); …"

- **NEW** `lan-mouse-gtk/src/clipboard_privacy_window.rs` and
  `add_suppressed_app_window.rs` — wrapper widgets following the
  `AuthorizationWindow` pattern.

- `lan-mouse-gtk/src/lib.rs` — handle
  `FrontendEvent::SuppressedAppsUpdated` and
  `FrontendEvent::RunningApps`; route to the privacy window.

- `lan-mouse-gtk/src/window.rs` — wire the "Manage" suffix button
  to open `ClipboardPrivacyWindow`.

## Phase 6 — test + ship

**Unit tests** (in respective crates):
- `AppIdent` parsing/serialization round-trips.
- `frontmost_app()` smoke tests on each supported platform (best-
  effort; some require a windowed environment).
- `recent_forwarded` LRU-style map: insert, eviction at TTL.

**Manual test plan** (in the PR description):
- 1Password on macOS: copy a password while clipboard sync is on
  with a peer; verify password does NOT propagate (concealed-type
  auto-detect).
- 1Password on Linux/Wayland (Hyprland or Sway): same; verify
  app-id-based suppression catches it.
- 1Password on Windows: same; verify exe-name suppression catches it.
- Add a non-password-manager app to the suppression list; copy from
  it; verify suppression.
- Remove the same app from the list; copy again; verify propagation
  resumes.
- Per-pair toggle off (either send or receive): verify no
  propagation.
- 3-peer fan-out: A → B → C, verify C receives once and doesn't
  echo back to A.
- Restart daemon; verify suppression list and per-pair toggles
  persist via `config.toml`.

**CI:** all platforms green before tag.

## Open questions / decisions still to make

(All sign-offs done as of plan creation. Section is intentionally empty;
re-add here if anything comes up during implementation.)

## macOS TODOs (deferred to a follow-up build pass on a Mac)

Phase 4 landed Linux (Hyprland + Sway + X11) and Windows
implementations of `frontmost_app::frontmost_app()` /
`list_running_apps()`. macOS is currently a stub returning
`None` / `Vec::new()`. To finish:

1. **Frontmost app**:
   `NSWorkspace.frontmostApplication.bundleIdentifier`. Either pull
   in `objc2` + `objc2-app-kit` (clean Rust bindings, ~10 LOC)
   or shell out to:
   ```sh
   osascript -e 'tell application "System Events" to get bundle identifier of first application process whose frontmost is true'
   ```
   The shell-out has ~50ms latency — fine for the 500ms clipboard
   poll. The bindings approach is preferred if we expect concealed-
   type detection too (next bullet).

2. **Running apps**:
   `NSWorkspace.runningApplications` map → bundle IDs.

3. **Concealed-type auto-suppression**:
   `NSPasteboard.generalPasteboard.types` checking for
   `org.nspasteboard.ConcealedType` UTI. Layer this on top of the
   user-maintained suppression list; when present, drop the change
   without consulting the list (and without
   `update_last_content`). Same Objective-C bridge as #1, so worth
   doing in the same patch.

The bundle of changes lives in `input-capture/src/frontmost_app.rs`
under `#[cfg(target_os = "macos")]`. The user-maintained list +
manual entry already work cross-platform, so macOS users can still
exercise the feature today by typing bundle IDs into the manual
entry tab — the auto-detect is the missing piece.

## Reference links

- PR #327 (Daniel Nakov): https://github.com/feschber/lan-mouse/pull/327
- PR #435 (per-pair scroll/sensitivity, our reference architecture):
  https://github.com/feschber/lan-mouse/pull/435
- Existing per-pair implementation in this codebase:
  - Schema: `lan-mouse-ipc/src/lib.rs::IncomingPeerConfig`
  - Receive-side gate pattern: `src/emulation.rs::ListenTask::post_processing_for_addr`
  - GTK row pattern: `lan-mouse-gtk/src/key_row.rs`,
    `lan-mouse-gtk/src/client_row.rs`
- Existing modal pattern: `lan-mouse-gtk/src/authorization_window.rs`

## Execution checkpoints (suggested human-review boundaries)

1. After Phase 1 commit lands: confirm primitives compile clean and
   `cargo test` passes.
2. After Phase 2: smoke-test 2-peer clipboard sync end-to-end before
   layering UI.
3. After Phase 3: confirm GTK toggles round-trip and that
   `config.toml` reflects the user's choices.
4. After Phase 4: confirm `frontmost_app()` works on at least one
   supported platform; add unit tests for the others.
5. After Phase 5: live-test the modal flow on macOS (password
   manager auto-suppression catches 1Password without manual config).
6. Before opening the PR: run the full manual test plan from Phase 6.
