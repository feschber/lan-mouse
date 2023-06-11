# Lan Mouse Share
Goal of this project is to be an open-source replacement for proprietary tools like [Synergy](https://symless.com/synergy), [Share Mouse](https://www.sharemouse.com/de/).

Focus lies on performance and a clean, manageable implementation that can easily be expanded to support additional backends like e.g. Android, iOS, ... .

Of course ***blazingly fast™*** and stable, because it's written in rust.

For an alternative (with slightly different goals) you may check out [Input Leap](https://github.com/input-leap).

## Configuration
Configuration is done through the file `config.toml`,
which must be located in the current working directory when
executing lan-mouse.

### Example config
A minimal config file could look like this:

```toml
[left]
host_name = "my-laptop"
```

Where `left` can be either `left`, `right`, `top` or `bottom`.


### Additional options
Additionally
- a preferred backend
- a port override for the default port (4242)

can be specified.

Supported backends currently include "wlroots", "x11" and "windows".

These two options can also be specified via the commandline
options `--backend` and `--port` respectively.

## Build and Run
Build only
```sh
cargo build --release
```

Run
```sh
cargo run --release
```

### Conditional Compilation

Currently only x11, wayland and windows are supported backends,
Depending on the toolchain used, support for other platforms is omitted
automatically (it does not make sense to build a Windows `.exe` with
support for x11 and wayland backends).

However one might still want to omit support for e.g. wayland or x11 on
a Linux system.

This is possible through
[cargo features](https://doc.rust-lang.org/cargo/reference/features.html)

E.g. if only wayland support is needed, the following command produces
an executable with just support for wayland:
```sh
cargo build --no-default-features --features wayland
```

## OS Support

The following table shows support for Event receiving and event Emitting
on different operating systems:

| Backend                   | Event Receiving          | Event Emitting                       |
|---------------------------|--------------------------|--------------------------------------|
| Wayland (wlroots)         | :heavy_check_mark:       | :heavy_check_mark:                   |
| Wayland (KDE)             | WIP                      | :heavy_check_mark:                   |
| Wayland (Gnome)           | TODO (libei support)     | TODO (wlr-layer-shell not supported) |
| X11                       | WIP                      | TODO                                 |
| Windows                   | needs improvements       | TODO                                 |
| MacOS                     | TODO (I dont own a Mac)  | TODO (I dont own a Mac)              |

## Wayland compositor support
### Input Emulation (for receiving events)
On wayland input-emulation is in an early/unstable state as of writing this.

Different compositors have different ways of enabling input emulation:

Most wlroots-based compositors like Hyprland and Sway support the following
unstable wayland protocols for keyboard and mouse emulation:
- [virtual-keyboard-unstable-v1](https://wayland.app/protocols/virtual-keyboard-unstable-v1)
- [wlr-virtual-pointer-unstable-v1](https://wayland.app/protocols/wlr-virtual-pointer-unstable-v1) are used to emulate input on wlroots compositors

KDE also has a protocol for input emulation ([kde-fake-input](https://wayland.app/protocols/kde-fake-input)), it is however not exposed to
third party apps, so the recommended way of enabling input emulation in KDE is the
[freedesktop remote-desktop-portal](https://flatpak.github.io/xdg-desktop-portal/#gdbus-org.freedesktop.portal.RemoteDesktop).

Gnome uses [libei](https://gitlab.freedesktop.org/libinput/libei) for input emulation,
which has the goal to become the general approach for emulating Input on wayland.

|  Required Protocols  (Event Receiving) | Sway               | Kwin                 | Gnome                |
|----------------------------------------|--------------------|----------------------|----------------------|
| wlr-virtual-pointer-unstable-v1        | :heavy_check_mark: | :x:                  | :x:                  |
| virtual-keyboard-unstable-v1           | :heavy_check_mark: | :x:                  | :x:                  |
| ~fake-input~                           | :x:                | ~:heavy_check_mark:~ | :x:                  |

### Input capture

To capture mouse and keyboard input, a few things are necessary:
- Displaying an immovable surface at screen edges
- Locking the mouse in place
- (optionally but highly recommended) reading unaccelerated mouse input

|  Required Protocols  (Event Emitting)  | Sway               | Kwin                 | Gnome                |
|----------------------------------------|--------------------|----------------------|----------------------|
| pointer-constraints-unstable-v1        | :heavy_check_mark: | :heavy_check_mark:   | :heavy_check_mark:   |
| relative-pointer-unstable-v1           | :heavy_check_mark: | :heavy_check_mark:   | :heavy_check_mark:   |
| keyboard-shortcuts-inhibit-unstable-v1 | :heavy_check_mark: | :heavy_check_mark:   | :heavy_check_mark:   |
| wlr-layer-shell-unstable-v1            | :heavy_check_mark: | :heavy_check_mark:   | :x:                  |

The [zwlr\_virtual\_pointer\_manager\_v1](wlr-virtual-pointer-unstable-v1) is required
to display surfaces on screen edges and used to display the immovable window on
both wlroots based compositors and KDE.

Gnome unfortunately does not support this protocol
and [likely won't ever support it](https://gitlab.gnome.org/GNOME/gnome-shell/-/issues/1141).

So there is currently no way of doing this in Wayland, aside from a custom Gnome-Shell
extension, which is not a very elegant solution.

This is to be looked into in the future.

~In order for layershell surfaces to be able to lock the pointer using the pointer\_constraints protocol [this patch](https://github.com/swaywm/sway/pull/7178) needs to be applied to sway.~
(this works natively on sway versions >= 1.8)

## Windows support
Currently windows can receive mouse and keyboard events, however unlike
with the wlroots back-end,

the scancodes are not translated between keyboard layouts.

Event emitting is WIP.


## TODOS
- [x] Capture the actual mouse events on the server side via a wayland client and send them to the client
- [x] Mouse grabbing
- [x] Window with absolute position -> wlr\_layer\_shell
- [x] DNS resolving
- [x] Keyboard support
- [x] Scrollwheel support
- [x] Button support
- [ ] Latency measurement + logging
- [ ] Bandwidth usage approximation + logging
- [ ] Multiple IP addresses -> check which one is reachable
- [x] Merge server and client -> Both client and server can send and receive events depending on what mouse is used where
- [ ] Liveness tracking (automatically ungrab mouse when client unreachable)
- [ ] Clipboard support
- [ ] Graphical frontend (gtk?)
- [ ] *Encrytion*
- [ ] Gnome Shell Extension (layer shell is not supported)
- [ ] respect xdg-config-home for config file location.

## Protocol
Currently *all* mouse and keyboard events are sent via **UDP** for performance reasons.
Each event is sent as one single datagram, currently without any acknowledgement to guarantee 0% packet loss.
This means, any packet that is lost results in a discarded mouse / key event, which is ignored for now.

**UDP** also has the additional benefit that no reconnection logic is required.
Any client can just go offline and it will simply start working again as soon as it comes back online.

Additionally a tcp server is hosted for data that needs to be sent reliably (e.g. the keymap from the server or clipboard contents in the future) can be requested via a tcp connection.

## Bandwidth considerations
The most bandwidth is taken up by mouse events. A typical office mouse has a polling rate of 125Hz
while gaming mice typically have a much higher polling rate of 1000Hz.
A mouse Event consists of 21 Bytes:
- 1 Byte for the event type enum,
- 4 Bytes (u32) for the timestamp,
- 8 Bytes (f64) for dx,
- 8 Bytes (f64) for dy.

Additionally the IP header with 20 Bytes and the udp header with 8 Bytes take up another 28 Byte.
So in total there is 49 * 1000 Bytes/s for a 1000Hz gaming mouse.
This makes for a bandwidth requirement of 392 kbit/s in total _even_ for a high end gaming mouse.
So bandwidth is a non-issue.

Larger data chunks, like the keymap are offered by the server via tcp listening on the same port.
This way we dont need to implement any congestion control and leave this up to tcp.
In the future this can be used for e.g. clipboard contents as well.

## Packets per Second
While on LAN the performance is great,
some WIFI cards seem to struggle with the amount of packets per second,
particularly on high-end gaming mice with 1000Hz+ polling rates.

The plan is to implement a way of accumulating packets and sending them as
one single key event to reduce the packet rate (basically reducing the polling
rate artificially).

The way movement data is currently sent is also quite wasteful since even a 16bit integer
is likely enough to represent even the fastest possible mouse movement.
A different encoding that is more efficient for smaller values like
[Protocol Buffers](https://protobuf.dev/programming-guides/encoding/)
would be a better choice for the future and could also help for WIFI connections.

## Security
Sending key and mouse event data over the local network might not be the biggest security concern but in any public network or business environment it's *QUITE* a problem to basically broadcast your keystrokes.
- There should be an encryption layer below the application to enable a secure link.
- The encryption keys could be generated by the graphical frontend.
