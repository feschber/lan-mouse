# General Software Architecture

## Peer Address and Connection Recovery

Activating a client starts its outgoing connection without waiting for captured
input or capture authorization. The service and capture task share a control
handle: activation and send fallback both ensure the same per-client worker.
That worker owns the connection, heartbeat, and recovery lifecycle.

```mermaid
flowchart LR
    Activate --> Worker["One worker per client"]
    SendFallback["Send fallback"] --> Worker
    Worker --> Resolve
    Resolve --> Connect
    Connect -->|failure| Backoff
    Backoff --> Resolve
    Connect -->|success| Connected
    Connected -->|transport lost| Backoff
```

Before every attempt, hostname clients use the existing asynchronous resolver.
The resolver uses the operating system's name-service stack, including supported
Bonjour/Avahi `.local` names. Fresh results replace cached DNS addresses; explicit
fixed IPs remain additional candidates. Configure a resolvable hostname and leave
fixed IPs empty when the peer's address is dynamic. A fixed IP alone cannot
discover its replacement.

Retries continue without further input, starting at one second and doubling to
a maximum of thirty seconds. A responding connection resets the delay.
`Pong(false)` means the peer's emulation is disabled, not that its transport
should be reconnected. Input that could not be sent is not queued for replay.

Preconnection reduces first-crossing delays; it does not guarantee the peer is
ready. Sends can still fail while connection setup is pending or remote input
emulation is disabled. Establishing a connection does not send an `Enter` or
pointer event and does not take control of the peer.

Endpoint edits restart pending work. Deactivation, deletion, and daemon shutdown
cancel it. Late DNS results and cleanup from an old connection cannot overwrite
an edited or replacement client. EOF stops both receive paths without replaying
the previous frame. Existing protocol and certificate-authorization behavior
are unchanged. Network recovery does not grant or restore portal permissions.

## Events

Each instance of lan-mouse can emit and receive events, where
an event is either a mouse or keyboard event for now.

The general Architecture is shown in the following flow chart:
```mermaid
graph TD
    A[Wayland Backend] -->|WaylandEvent| D{Input}
    B[X11 Backend] -->|X11Event| D{Input}
    C[Windows Backend] -->|WindowsEvent| D{Input}
    D -->|Abstract Event| E[Emitter]
    E -->|Udp Event| F[Receiver]
    F -->|Abstract Event| G{Dispatcher}
    G -->|Wayland Event| H[Wayland Backend]
    G -->|X11 Event| I[X11 Backend]
    G -->|Windows Event| J[Windows Backend]
```

### Input
The input component is responsible for translating inputs from a given backend
to a standardized format and passing them to the event emitter.

### Emitter
The event emitter serializes events and sends them over the network
to the correct client.

### Receiver
The receiver receives events over the network and deserializes them into
the standardized event format.

### Dispatcher
The dispatcher component takes events from the event receiver and passes them
to the correct backend corresponding to the type of client.


## Requests

// TODO this currently works differently

Aside from events, requests can be sent via a simple protocol.
For this, a simple tcp server is listening on the same port as the udp
event receiver and accepts requests for connecting to a device or to
request the keymap of a device.

```mermaid
sequenceDiagram
    Alice->>+Bob: Request Connection (secret)
    Bob-->>-Alice: Ack (Keyboard Layout)
```

## Problems
The general Idea is to have a bidirectional connection by default, meaning
any connected device can not only receive events but also send events back.

This way when connecting e.g. a PC to a Laptop, either device can be used
to control the other.

It needs to be ensured, that whenever a device is controlled the controlled
device does not transmit the events back to the original sender.
Otherwise events are multiplied and either one of the instances crashes.

To keep the implementation of input backends simple this needs to be handled
on the server level.

## Device State - Active and Inactive
To solve this problem, each device can be in exactly two states:

Either events are sent or received.

This ensures that
- a) Events can never result in a feedback loop.
- b) As soon as a virtual input enters another client, lan-mouse will stop receiving events,
which ensures clients can only be controlled directly and not indirectly through other clients.

