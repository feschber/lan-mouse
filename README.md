# Lan Mouse [WIP]
Goal of this project is to be an open-source replacement for tools like [Synergy](https://symless.com/synergy) or [Share Mouse](https://www.sharemouse.com/de/) on wayland compositors.

## Very much alpha state
The protocol used for the virtual mouse driver is currently unstable and only supported by wlroots:
[wlr-virtual-pointer-unstable-v1](wlr-virtual-pointer-unstable-v1)

Currently the mouse moves in a circle when receiving a(ny) udp packet on port 42069.

## TODOS:
- Capture the actual mouse events on the server side and send them to the client. Ideally via some 1 pixel wide transparent window that captures the mouse on the server side and then sends its events to the client.
- Keyboard support
- Add support for clipboard contents
- Graphical frontend (gtk?)
