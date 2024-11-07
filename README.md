# Lan Mouse
Lan Mouse is a *cross-platform* mouse and keyboard sharing software similar to universal-control on Apple devices.
It allows for using multiple PCs via a single set of mouse and keyboard.
This is also known as a Software KVM switch.

Goal of this project is to be an open-source alternative to proprietary tools like [Synergy 2/3](https://symless.com/synergy), [Share Mouse](https://www.sharemouse.com/de/)
and an alternative to other open source tools like [Deskflow](https://github.com/deskflow/deskflow) or [Input Leap](https://github.com/input-leap) (Synergy fork).

Focus lies on performance, ease of use and a maintainable implementation that can be expanded to support additional backends for e.g. Android, iOS, ... in the future.

***blazingly fast™*** because it's written in rust.

- _Now with a gtk frontend_

<picture>
    <source media="(prefers-color-scheme: dark)" srcset="/screenshots/dark.png?raw=true">
    <source media="(prefers-color-scheme: light)" srcset="/screenshots/light.png?raw=true">
    <img alt="Screenshot of Lan-Mouse" srcset="/screenshots/dark.png">
</picture>


> [!WARNING]
> DISCLAIMER:
> Until [#200](https://github.com/feschber/lan-mouse/pull/200) is merged, all network traffic is **unencrypted** and sent in **plaintext**!
>
> A malicious actor with access to the network could read input data or send input events with spoofed IPs to take control over a device.
>
> Therefore you should only use this tool in your local network with trusted devices.
> I take no responsibility for any security breaches!


## OS Support

Most current desktop environments and operating systems are fully supported, this includes
- GNOME >= 45
- KDE Plasma >= 6.1
- Most wlroots based compositors, including Sway (>= 1.8), Hyprland and Wayfire
- Windows
- MacOS


### Caveats / Known Issues

> [!Important]
> - **X11** currently only has support for input emulation, i.e. can only be used on the receiving end.
>
> - **Sway / wlroots**: Wlroots based compositors without libei support on the receiving end currently do not handle modifier events on the client side.
> This results in CTRL / SHIFT / ALT / SUPER keys not working with a sending device that is NOT using the `layer-shell` backend
>
> - **Wayfire**: If you are using [Wayfire](https://github.com/WayfireWM/wayfire), make sure to use a recent version (must be newer than October 23rd) and **add `shortcuts-inhibit` to the list of plugins in your wayfire config!**
> Otherwise input capture will not work.
>
> - **Windows**: The mouse cursor will be invisible when sending input to a Windows system if
> there is no real mouse connected to the machine.

For more detailed information about os support see [Detailed OS Support](#detailed-os-support)


## Installation

<details>
    <summary>Arch Linux</summary>

Lan Mouse can be installed from the [official repositories](https://archlinux.org/packages/extra/x86_64/lan-mouse/):

```sh
pacman -S lan-mouse
```

The prerelease version (following `main`) is available on the AUR:

```sh
paru -S lan-mouse-git
```
</details>


<details>
    <summary>Nix (OS)</summary>

- nixpkgs: [search.nixos.org](https://search.nixos.org/packages?channel=unstable&show=lan-mouse&from=0&size=50&sort=relevance&type=packages&query=lan-mouse)
- flake: [README.md](./nix/README.md)
</details>



<details>
    <summary>Manual Installation</summary>

First make sure to [install the necessary dependencies](#installing-dependencies).

Precompiled release binaries for Windows, MacOS and Linux are available in the [releases section](https://github.com/feschber/lan-mouse/releases).
For Windows, the depenedencies are included in the .zip file, for other operating systems see [Installing Dependencies](#installing-dependencies).

Alternatively, the `lan-mouse` binary can be compiled from source (see below).

### Installing desktop file, app icon and firewall rules (optional)
```sh
# install lan-mouse (replace path/to/ with the correct path)
sudo cp path/to/lan-mouse /usr/local/bin/

# install app icon
sudo mkdir -p /usr/local/share/icons/hicolor/scalable/apps
sudo cp lan-mouse-gtk/resources/de.feschber.LanMouse.svg /usr/local/share/icons/hicolor/scalable/apps

# update icon cache
gtk-update-icon-cache /usr/local/share/icons/hicolor/

# install desktop entry
sudo mkdir -p /usr/local/share/applications
sudo cp de.feschber.LanMouse.desktop /usr/local/share/applications

# when using firewalld: install firewall rule
sudo cp firewall/lan-mouse.xml /etc/firewalld/services
# -> enable the service in firewalld settings
```

Instead of downloading from the releases, the `lan-mouse` binary
can be easily compiled via cargo or nix:

### Compiling and installing manually:
```sh
# compile in release mode
cargo build --release

# install lan-mouse
sudo cp target/release/lan-mouse /usr/local/bin/
```

### Compiling and installing via cargo:
```sh
# will end up in ~/.cargo/bin
cargo install lan-mouse
```

### Compiling and installing via nix:
```sh
# you can find the executable in result/bin/lan-mouse
nix-build
```
### Conditional compilation
Support for other platforms is omitted automatically based on the active
rust toolchain.

Additionally, available backends and frontends can be configured manually via
[cargo features](https://doc.rust-lang.org/cargo/reference/features.html).

E.g. if only wayland support is needed, the following command produces
an executable with just support for wayland:
```sh
cargo build --no-default-features --features wayland
```
For a detailed list of available features, checkout the [Cargo.toml](./Cargo.toml)
</details>




## Installing Dependencies for Development / Compiling from Source
<details>
    <summary>MacOS</summary>

```sh
brew install libadwaita pkg-config
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
    <summary>Nix</summary>
    
```sh
nix-shell .
```
</details>
<details>
    <summary>Nix (flake)</summary>
    
```sh
nix develop
```
</details>

<details>
    <summary>Windows</summary>

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
gvsbuild build gtk4 libadwaita librsvg adwaita-icon-theme
```

- **Make sure to add the directory** `C:\gtk-build\gtk\x64\release\bin`
[**to the `PATH` environment variable**]((https://learn.microsoft.com/en-us/previous-versions/office/developer/sharepoint-2010/ee537574(v=office.14))). Otherwise the project will fail to build.

To avoid building GTK from source, it is possible to disable
the gtk frontend (see conditional compilation).
</details>

## Usage
<details>
    <summary>Gtk Frontend</summary>

By default the gtk frontend will open when running `lan-mouse`.

To add a new connection, simply click the `Add` button on *both* devices,
enter the corresponding hostname and activate it.

If the mouse can not be moved onto a device, make sure you have port `4242` (or the one selected)
opened up in your firewall.
</details>

<details>
    <summary>Command Line Interface</summary>

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
</details>

<details>
    <summary>Daemon Mode</summary>

Lan Mouse can be launched in daemon mode to keep it running in the background (e.g. for use in a systemd-service).

To do so, add `--daemon` to the commandline args:

```sh
lan-mouse --daemon
```

In order to start lan-mouse with a graphical session automatically,
the [systemd-service](service/lan-mouse.service) can be used:

Copy the file to `~/.config/systemd/user/` and enable the service:

```sh
cp service/lan-mouse.service ~/.config/systemd/user
systemctl --user daemon-reload
systemctl --user enable --now lan-mouse.service
```
</details>

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
- [x] MacOS Input Capture
- [x] Windows Input Capture
- [ ] *Encryption* (WIP)
- [ ] X11 Input Capture
- [ ] Latency measurement and visualization
- [ ] Bandwidth usage measurement and visualization
- [ ] Clipboard support


## Detailed OS Support

In order to use a device for sending events, an **input-capture** backend is required, while receiving events requires
a supported **input-emulation** *and* **input-capture** backend.

A suitable backend is chosen automatically based on the active desktop environment / compositor.

The following sections detail the emulation and capture backends provided by lan-mouse and their support in desktop environments / operating systems.

### Input Emulation Support

| Desktop / Backend         | layer-shell              | libei                    | windows                  |   macos                                | x11 |
|---------------------------|--------------------------|--------------------------|--------------------------|----------------------------------------|-----|
| Wayland (wlroots)         | :heavy_check_mark:       |                          |                          |                                        |     |
| Wayland (KDE)             | :heavy_check_mark:       | :heavy_check_mark:       |                          |                                        |     |
| Wayland (Gnome)           |                          | :heavy_check_mark:       |                          |                                        |     |
| Windows                   |                          |                          | :heavy_check_mark:       |                                        |     |
| MacOS                     |                          |                          |                          |   :heavy_check_mark:                   |     |
| X11                       |                          |                          |                          |                                        | WIP |

- `wlroots`: This backend makes use of the [wlr-virtual-pointer-unstable-v1](https://wayland.app/protocols/wlr-virtual-pointer-unstable-v1) and [virtual-keyboard-unstable-v1](https://wayland.app/protocols/virtual-keyboard-unstable-v1) protocols and is supported by most wlroots based compositors.
- `libei`: This backend uses [libei](https://gitlab.freedesktop.org/libinput/libei) and is supported by GNOME >= 45 or KDE Plasma >= 6.1.
- `xdp`: This backend uses the [freedesktop remote-desktop-portal](https://flatpak.github.io/xdg-desktop-portal/#gdbus-org.freedesktop.portal.RemoteDesktop) and is supported on GNOME and Plasma.
- `x11`: Backend for X11 sessions.
- `windows`: Backend for Windows.
- `macos`: Backend for MacOS.



### Input Capture Support

| Desktop / Backend         | wlroots                  | libei                    | remote-desktop portal    | windows                  |   macos                                | x11                |
|---------------------------|--------------------------|--------------------------|--------------------------|--------------------------|----------------------------------------|--------------------|
| Wayland (wlroots)         | :heavy_check_mark:       |                          |                          |                          |                                        |                    |
| Wayland (KDE)             |                          | :heavy_check_mark:       | :heavy_check_mark:       |                          |                                        |                    |
| Wayland (Gnome)           |                          | :heavy_check_mark:       | :heavy_check_mark:       |                          |                                        |                    |
| Windows                   |                          |                          |                          | :heavy_check_mark:       |                                        |                    |
| MacOS                     |                          |                          |                          |                          |   :heavy_check_mark:                   |                    |
| X11                       |                          |                          |                          |                          |                                        | :heavy_check_mark: |

- `layer-shell`: This backend creates a single pixel wide window on the edges of Displays to capture the cursor using the [layer-shell protocol](https://wayland.app/protocols/wlr-layer-shell-unstable-v1).
- `libei`: This backend uses [libei](https://gitlab.freedesktop.org/libinput/libei) and is supported by GNOME >= 45 or KDE Plasma >= 6.1.
- `windows`: Backend for input capture on Windows.
- `macos`: Backend for input capture on MacOS.
- `x11`: TODO (not yet supported)



