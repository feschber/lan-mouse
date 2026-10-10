# Isolated wlroots output-recovery test

Run from a Wayland desktop with virtual-pointer and virtual-keyboard protocols:

```sh
python3 tests/wayland-recovery/run.py
```

Python 3 is required only for this test harness. There are no runtime dependency
changes. The runner starts a temporary Unix-socket proxy for the test client,
then runs the ignored `real_wayland_stall_disables_then_rebuilds_emulation` test.
The proxy pauses reads on this isolated connection for 1200ms and forwards file
descriptors using SCM_RIGHTS. It never pauses the compositor or the application's
normal connections. All injected input is zero-distance motion; no keys or
buttons are sent.

The test expects Enabled, Disabled after a bounded output stall, then Enabled
after rebuilding. New input must be accepted after recovery. Temporary sockets
and flag files are removed on exit. WAYLAND_DISPLAY and XDG_RUNTIME_DIR must
describe a live desktop session. Use `--cargo /path/to/cargo` or `--timeout 120`
if needed for your build environment.
