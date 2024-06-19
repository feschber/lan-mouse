# Lan Mouse
Lan Mouse is a mouse and keyboard sharing software similar to universal-control on Apple devices.
It allows for using multiple pcs with a single set of mouse and keyboard.
This is also known as a Software KVM switch.

The primary target is Wayland on Linux but Windows and MacOS and Linux on Xorg have partial support as well (see below for more details).

- _Now with a gtk frontend_

<picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://github.com/feschber/lan-mouse/assets/40996949/016a06a9-76db-4951-9dcc-127d012c59df">
    <source media="(prefers-color-scheme: light)" srcset="https://github.com/feschber/lan-mouse/assets/40996949/d6318340-f811-4e16-9d6e-d1b79883c709">
    <img alt="Screenshot of Lan-Mouse" srcset="https://github.com/feschber/lan-mouse/assets/40996949/016a06a9-76db-4951-9dcc-127d012c59df">
</picture>


Goal of this project is to be an open-source replacement for proprietary tools like [Synergy 2/3](https://symless.com/synergy), [Share Mouse](https://www.sharemouse.com/de/).

Focus lies on performance and a clean, manageable implementation that can easily be expanded to support additional backends like e.g. Android, iOS, ... .

***blazingly fast™*** because it's written in rust.

For an alternative (with slightly different goals) you may check out [Input Leap](https://github.com/input-leap).


> [!WARNING]
> Since this tool has gained a bit of popularity over the past couple of days:
>
> All network traffic is currently **unencrypted** and sent in **plaintext**.
>
> A malicious actor with access to the network could read input data or send input events with spoofed IPs to take control over a device.
>
> Therefore you should only use this tool in your local network with trusted devices for now
> and I take no responsibility for any leakage of data!


## OS Support

The following table shows support for input emulation (to emulate events received from other clients) and
input capture (to send events *to* other clients) on different operating systems:

| OS / Desktop Environment  | input emulation          | input capture                        |
|---------------------------|--------------------------|--------------------------------------|
| Wayland (wlroots)         | :heavy_check_mark:       | :heavy_check_mark:                   |
| Wayland (KDE)             | :heavy_check_mark:       | :heavy_check_mark:                   |
| Wayland (Gnome)           | :heavy_check_mark:       | :heavy_check_mark: (starting at GNOME 45) |
| Windows                   | :heavy_check_mark:       | :heavy_check_mark:                   |
| X11                       | :heavy_check_mark:       | WIP                                  |
| MacOS                     | :heavy_check_mark:       | WIP                                  |

> [!Important]
> Gnome -> Sway only partially works (modifier events are not handled correctly)

> [!Important]
> **Wayfire**
>
> If you are using [Wayfire](https://github.com/WayfireWM/wayfire), make sure to use a recent version (must be newer than October 23rd) and **add `shortcuts-inhibit` to the list of plugins in your wayfire config!**
> Otherwise input capture will not work.

## Installation
### Install via cargo
```sh
cargo install lan-mouse
```

### Download from Releases
Precompiled release binaries for Windows, MacOS and Linux are available in the [releases section](https://github.com/feschber/lan-mouse/releases).

For Windows, the depenedencies are included in the .zip file, for other operating systems see [Installing Dependencies](#installing-dependencies).

### Arch Linux

Lan Mouse can be installed from the [official repositories](https://archlinux.org/packages/extra/x86_64/lan-mouse/):

```sh
pacman -S lan-mouse
```

It is also available on the AUR:

```sh
# git version (includes latest changes)
paru -S lan-mouse-git

# alternatively
paru -S lan-mouse-bin
```

### Nix
- nixpkgs: [search.nixos.org](https://search.nixos.org/packages?channel=unstable&show=lan-mouse&from=0&size=50&sort=relevance&type=packages&query=lan-mouse)
- flake: [README.md](./nix/README.md)


### Manual Installation

First make sure to [install the necessary dependencies](#installing-dependencies).

Build in release mode:
```sh
cargo build --release
```

Run directly:
```sh
cargo run --release
```

Install the files:
```sh
# install lan-mouse
sudo cp target/release/lan-mouse /usr/local/bin/

# install app icon
sudo mkdir -p /usr/local/share/icons/hicolor/scalable/apps
sudo cp resources/de.feschber.LanMouse.svg /usr/local/share/icons/hicolor/scalable/apps

# update icon cache
gtk-update-icon-cache /usr/local/share/icons/hicolor/

# install desktop entry
sudo mkdir -p /usr/local/share/applications
sudo cp de.feschber.LanMouse.desktop /usr/local/share/applications

# when using firewalld: install firewall rule
sudo cp firewall/lan-mouse.xml /etc/firewalld/services
# -> enable the service in firewalld settings
```

### Conditional Compilation

Currently only x11, wayland, windows and MacOS are supported backends.
Depending on the toolchain used, support for other platforms is omitted
automatically (it does not make sense to build a Windows `.exe` with
support for x11 and wayland backends).

However one might still want to omit support for e.g. wayland, x11 or libei on
a Linux system.

This is possible through
[cargo features](https://doc.rust-lang.org/cargo/reference/features.html).

E.g. if only wayland support is needed, the following command produces
an executable with just support for wayland:
```sh
cargo build --no-default-features --features wayland
```
For a detailed list of available features, checkout the [Cargo.toml](./Cargo.toml)


## Installing Dependencies
<details>
    <summary>MacOS</summary>

```sh
brew install libadwaita
```
</details>

<details>
    <summary>Ubuntu and derivatives</summary>

```sh
sudo apt install libadwaita-1-dev libgtk-4-dev libx11-dev libxtst-dev
```
</details>

<details>
    <summary>Arch and derivatives</summary>

```sh
sudo pacman -S libadwaita gtk libx11 libxtst
```
</details>

<details>
    <summary>Fedora and derivatives</summary>

```sh
sudo dnf install libadwaita-devel libXtst-devel libX11-devel
```
</details>
<details>
    <summary>Windows</summary>

> [!NOTE]
> This is only necessary when building lan-mouse from source. The windows release comes with precompiled gtk dlls.

- First install [Rust](https://www.rust-lang.org/tools/install).

- Then follow the instructions at [gtk-rs.org](https://gtk-rs.org/gtk4-rs/stable/latest/book/installation_windows.html)

*TLDR:*

Build gtk from source

- The following commands should be run in an **admin power shell** instance:
```sh
# install chocolatey
Set-ExecutionPolicy Bypass -Scope Process -Force; iex ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))

# install gvsbuild dependencies
choco install python git msys2 visualstudio2022-workload-vctools
```

- The following commands should be run in a **regular power shell** instance:

```sh
# install gvsbuild with python
python -m pip install --user pipx
python -m pipx ensurepath
```

- Relaunch your powershell instance so the changes in the environment are reflected.
```sh
pipx install gvsbuild

# build gtk + libadwaita
gvsbuild build gtk4 libadwaita librsvg
```

- **Make sure to add the directory** `C:\gtk-build\gtk\x64\release\bin`
[**to the `PATH` environment variable**]((https://learn.microsoft.com/en-us/previous-versions/office/developer/sharepoint-2010/ee537574(v=office.14))). Otherwise the project will fail to build.

To avoid building GTK from source, it is possible to disable
the gtk frontend (see conditional compilation below).
</details>

## Usage
### Gtk Frontend
By default the gtk frontend will open when running `lan-mouse`.

To add a new connection, simply click the `Add` button on *both* devices,
enter the corresponding hostname and activate it.

If the mouse can not be moved onto a device, make sure you have port `4242` (or the one selected)
opened up in your firewall.

### Command Line Interface
The cli interface can be enabled using `--frontend cli` as commandline arguments.
Type `help` to list the available commands.

E.g.:
```sh
$ cargo run --release -- --frontend cli
(...)
> connect <host> left|right|top|bottom
(...)
> list
(...)
> activate 0
```

### Daemon
Lan Mouse can be launched in daemon mode to keep it running in the background.
To do so, add `--daemon` to the commandline args:

```sh
$ cargo run --release -- --daemon
```

In order to start lan-mouse with a graphical session automatically,
the [systemd-service](service/lan-mouse.service) can be used:

Copy the file to `~/.config/systemd/user/` and enable the service:

```sh
cp service/lan-mouse.service ~/.config/systemd/user
systemctl --user daemon-reload
systemctl --user enable --now lan-mouse.service
```

## Configuration
To automatically load clients on startup, the file `$XDG_CONFIG_HOME/lan-mouse/config.toml` is parsed.
`$XDG_CONFIG_HOME` defaults to `~/.config/`.

To create this file you can copy the following example config:

### Example config
> [!TIP]
> key symbols in the release bind are named according
> to their names in [src/scancode.rs#L172](src/scancode.rs#L172).
> This is bound to change

```toml
# example configuration

# configure release bind
release_bind = [ "KeyA", "KeyS", "KeyD", "KeyF" ]

# optional port (defaults to 4242)
port = 4242
# # optional frontend -> defaults to gtk if available
# # possible values are "cli" and "gtk" 
# frontend = "gtk"

# define a client on the right side with host name "iridium"
[right]
# hostname
hostname = "iridium"
# activate this client immediately when lan-mouse is started
activate_on_startup = true
# optional list of (known) ip addresses
ips = ["192.168.178.156"]

# define a client on the left side with IP address 192.168.178.189
[left]
# The hostname is optional: When no hostname is specified,
# at least one ip address needs to be specified.
hostname = "thorium"
# ips for ethernet and wifi
ips = ["192.168.178.189", "192.168.178.172"]
# optional port
port = 4242
```

Where `left` can be either `left`, `right`, `top` or `bottom`.

## Roadmap
- [x] Graphical frontend (gtk + libadwaita)
- [x] respect xdg-config-home for config file location.
- [x] IP Address switching
- [x] Liveness tracking Automatically ungrab mouse when client unreachable
- [x] Liveness tracking: Automatically release keys, when server offline
- [x] MacOS KeyCode Translation
- [x] Libei Input Capture
- [ ] X11 Input Capture
- [ ] Windows Input Capture
- [ ] MacOS Input Capture
- [ ] Latency measurement and visualization
- [ ] Bandwidth usage measurement and visualization
- [ ] Clipboard support
- [ ] *Encryption*

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


## Wayland support
### Input Emulation (for receiving events)
On wayland input-emulation is in an early/unstable state as of writing this.

For this reason a suitable backend is chosen based on the active desktop environment / compositor.

Different compositors have different ways of enabling input emulation:

#### Wlroots
Most wlroots-based compositors like Hyprland and Sway support the following
unstable wayland protocols for keyboard and mouse emulation:
- [virtual-keyboard-unstable-v1](https://wayland.app/protocols/virtual-keyboard-unstable-v1)
- [wlr-virtual-pointer-unstable-v1](https://wayland.app/protocols/wlr-virtual-pointer-unstable-v1)

#### KDE
KDE also has a protocol for input emulation ([kde-fake-input](https://wayland.app/protocols/kde-fake-input)),
it is however not exposed to third party applications.

The recommended way to emulate input on KDE is the
[freedesktop remote-desktop-portal](https://flatpak.github.io/xdg-desktop-portal/#gdbus-org.freedesktop.portal.RemoteDesktop).

#### Gnome
Gnome uses [libei](https://gitlab.freedesktop.org/libinput/libei) for input emulation and capture,
which has the goal to become the general approach for emulating and capturing Input on Wayland.

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

~In order for layershell surfaces to be able to lock the pointer using the pointer\_constraints protocol [this patch](https://github.com/swaywm/sway/pull/7178) needs to be applied to sway.~
(this works natively on sway versions >= 1.8)

