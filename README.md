# Lan Mouse [WIP]
Goal of this project is to be an open-source replacement for tools like [Synergy](https://symless.com/synergy) or [Share Mouse](https://www.sharemouse.com/de/) on wayland compositors.

## Very much alpha state
The protocol used for the virtual mouse driver is currently unstable and only supported by wlroots:
[wlr-virtual-pointer-unstable-v1](wlr-virtual-pointer-unstable-v1)

Currently the mouse moves in a circle when receiving a(ny) udp packet on port 42069.

## TODO
- [x] Capture the actual mouse events on the server side via a wayland client and send them to the client
- [x] Mouse grabbing
- [ ] Window with absolute position (wlr\_layer\_shell?)
- [x] DNS resolving
- [ ] Keyboard support
- [ ] Scrollwheel support
- [x] Button support
- [ ] Merge server and client
- [ ] Clipboard support
- [ ] Graphical frontend (gtk?)
- [ ] *Encrytion*

## Security
Sending key and mouse event data over the local network might not be the biggest security concern but in any public network it's QUITE a problem to basically broadcast your keystrokes.
- There should probably be an encryption layer using DTLS below the application to enable a secure link
- The keys could be generated via the graphical frontend
