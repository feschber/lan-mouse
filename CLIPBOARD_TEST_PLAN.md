# Per-pair clipboard sync — manual test plan

A self-contained checklist for verifying the feature on a fresh
build. Targeted at the PR description; one section per phase plus
a cross-cutting end-to-end suite at the bottom.

## 0. Setup

- Build the daemon: `cargo build --release -p lan-mouse`.
- Two peers reachable on the LAN, both authorized (existing
  fingerprint exchange flow). Call them **A** and **B**.
- After install/launch on either side, open the GUI and confirm
  the new "Clipboard Privacy" preferences group is visible at the
  bottom of the main window with subtitle "0 apps".

## 1. Per-pair gates default to false

- [ ] On A's GUI, expand B's row in **Outgoing Clients**. Confirm
      "Share Clipboard With This Peer" switch is **off**.
- [ ] On A's GUI, expand A-from-B's-perspective row in **Incoming
      Connections**. Confirm "Accept Clipboard From This Peer"
      switch is **off**.
- [ ] Inspect `~/.config/lan-mouse/config.toml` (Linux/macOS) or
      the equivalent on Windows. Confirm:
  - `clipboard_send` is absent from the `clients` entry (or `false`).
  - `clipboard_receive` is absent from the authorized-fingerprints
    entry (or `false`).
- [ ] Copy text on A. Confirm B's clipboard is unchanged.

## 2. Two-peer happy path

- [ ] On A: turn on "Share Clipboard With This Peer" for B.
- [ ] On B: turn on "Accept Clipboard From This Peer" for A.
- [ ] On A: copy `hello-from-A` (or any short distinctive string).
- [ ] On B: paste. Should produce `hello-from-A`.
- [ ] On B: copy `hello-from-B`. (`clipboard_send` is off on B,
      `clipboard_receive` is off on A.)
- [ ] On A: paste. Should NOT contain `hello-from-B` — both peers
      need their respective bits set in the right direction.

## 3. Toggle off mid-flight

- [ ] With section 2's setup still active and clipboards confirmed
      working A → B, turn OFF B's "Accept Clipboard From This Peer"
      for A.
- [ ] On A: copy `should-not-arrive`.
- [ ] On B: paste. Should NOT contain `should-not-arrive`.
- [ ] On A's daemon log (RUST_LOG=debug): expect a
      `dropping clipboard frame from <addr>: clipboard_receive
      disabled` line at debug level.
- [ ] Re-enable B's "Accept Clipboard From This Peer". Repeat the
      copy on A. Should arrive on B.

## 4. Per-pair persistence

- [ ] With both gates on, kill both daemons.
- [ ] Restart both daemons.
- [ ] In the GUI on each side, confirm the toggles are still on.
- [ ] Repeat the section 2 happy-path test without retoggling.

## 5. Three-peer fan-out

Set up a third peer **C** authorized with both A and B; turn on
all relevant `clipboard_send` / `clipboard_receive` pairs.

- [ ] Copy on A. Verify clipboard on B and C both update once each
      (no flapping, no echo).
- [ ] Daemon-log scan on each peer: expect at most ONE
      `forwarding clipboard` line per peer per copy event.
- [ ] Verify the round-trip from C does NOT re-trigger a copy on
      A — A's `recent_forwarded` gate suppresses the duplicate.

## 6. Loop prevention edge case

- [ ] On A: copy `cycle-test` repeatedly within ~1 second.
- [ ] B and C should each see the value but should NOT continue
      churning broadcasts among themselves after the initial fan-
      out (verify by tail of `RUST_LOG=trace lan-mouse 2>&1 |
      grep clipboard`).

## 7. Suppression list — manual entry path

- [ ] On A's GUI, click "Manage" in the Clipboard Privacy group.
      The modal opens.
- [ ] In "Add an App", pick the platform-appropriate kind
      (e.g. `Linux X11 (firefox)` if you're on X11), type
      `firefox` (or another app you have running), click "Add".
- [ ] Confirm the entry appears in the list above.
- [ ] Confirm the main-window subtitle now reads "1 app".
- [ ] Confirm `~/.config/lan-mouse/config.toml` has a
      `[[clipboard_suppress_apps]]` entry.
- [ ] Click the trash icon next to the entry. List goes empty;
      subtitle goes back to "0 apps".

## 8. Suppression actually suppresses (Linux, X11 / Wayland)

- [ ] Add the active terminal to A's suppression list (e.g.
      `gnome-terminal-server` on X11, `org.gnome.Terminal` /
      `kitty` / etc. on Wayland).
- [ ] Copy text from that terminal. Verify B's clipboard does NOT
      update.
- [ ] Open a different app (browser, text editor). Copy text from
      it. Verify B's clipboard DOES update.
- [ ] Daemon log on A (`RUST_LOG=debug`): expect
      `clipboard change suppressed (frontmost app …)` for the
      suppressed-app copies.

## 9. Suppression actually suppresses (Windows)

- [ ] On a Windows peer, add `notepad.exe` (or any open app's
      executable basename) to the suppression list.
- [ ] Copy from Notepad. Verify the receiving peer's clipboard
      does NOT update.
- [ ] Copy from a different app. Verify it DOES update.

## 10. Suppression — macOS (after macOS-side build pass)

The macOS frontmost-app stub returns `None`, so:

- [ ] Until the objc2 follow-up lands, manual entries in the list
      do not auto-detect — but a Mac peer broadcasting clipboard
      to a Linux/Windows peer will still reach the receive-side
      gate. Confirm the per-pair `clipboard_receive` toggle on the
      receiving peer continues to work as expected even when the
      sending peer is macOS.
- [ ] After the macOS follow-up lands (NSWorkspace.frontmost-
      Application.bundleIdentifier + concealed-type detection),
      repeat sections 8–9 with a macOS sender and a macOS-bundle
      entry.

## 11. Build / lint

- [ ] `cargo build --release -p lan-mouse` clean on Linux.
- [ ] `cargo build --release -p lan-mouse` clean on Windows.
- [ ] `cargo build --release -p lan-mouse` clean on macOS (after
      objc2 work).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
      clean.
- [ ] `cargo test --workspace` green (16 new unit tests covering
      AppIdent serde / matches / labels, IncomingPeerConfig legacy
      shapes, ClientConfig defaults, ProtoEvent::Clipboard codec
      round-trip + size cap + truncation, recent_forwarded TTL
      eviction, frontmost_app smoke).

## 12. Network constraints

- [ ] Copy a string just under 4 KiB. Verify it arrives intact.
- [ ] Copy a string just over 4 KiB. Daemon log on the sender
      should show
      `dropping oversize clipboard event for client <handle>:
      clipboard payload too large`. Receiver clipboard unchanged.

## 13. Compatibility with existing peer-version handshake

- [ ] Connect to a peer running an older `main`-tree build that
      doesn't know `EventType::Clipboard`. Send clipboard from
      the new build.
- [ ] Old peer should silently ignore the unknown event type
      (forward-compat handler in the receive loop). No
      disconnect, no log noise above debug level.
