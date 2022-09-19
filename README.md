# Lan Mouse Share
Goal of this project is to be an open-source replacement for tools like [Synergy](https://symless.com/synergy) or [Share Mouse](https://www.sharemouse.com/de/).
Currently only wayland is supported but I will take a look at xorg, windows & MacOS in the future.

## Very much alpha state
The protocols used for the virtual mouse and virtual keyboard drivers are currently unstable and only supported by wlroots:
[zwlr\_virtual\_pointer\_manager\_v1](wlr-virtual-pointer-unstable-v1)
[virtual-keyboard-unstable-v1](https://wayland.app/protocols/virtual-keyboard-unstable-v1)

In order for layershell surfaces to be able to lock the pointer using the pointer\_constraints protocol [this patch](https://github.com/swaywm/sway/pull/7178) needs to be applied to sway.

## TODO
- [x] Capture the actual mouse events on the server side via a wayland client and send them to the client
- [x] Mouse grabbing
- [x] Window with absolute position (wlr\_layer\_shell?)
- [x] DNS resolving
- [ ] Multiple IP addresses -> check which one is reachable
- [x] Keyboard support
- [x] Scrollwheel support
- [x] Button support
- [ ] Merge server and client -> Both client and server can send and receive events depending on what mouse is used where
- [ ] Liveness tracking (automatically ungrab mouse when client unreachable)
- [ ] Clipboard support
- [ ] Graphical frontend (gtk?)
- [ ] *Encrytion* -> likely DTLS

## Protocol considerations
Currently UDP is used exclusively for all events sent and / or received.
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


## Security
Sending key and mouse event data over the local network might not be the biggest security concern but in any public network it's QUITE a problem to basically broadcast your keystrokes.
- There should probably be an encryption layer using DTLS below the application to enable a secure link
- The keys could be generated via the graphical frontend
