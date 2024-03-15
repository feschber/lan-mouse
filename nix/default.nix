{
  rustPlatform,
  lib,
  pkgs,
}:
rustPlatform.buildRustPackage {
  pname = "lan-mouse";
  version = "0.6.0";

  nativeBuildInputs = with pkgs; [
    pkg-config
    cmake
    buildPackages.gtk4
  ];

  buildInputs = with pkgs; [
    xorg.libX11
    gtk4
    libadwaita
    xorg.libXtst
  ];

  src = builtins.path {
    name = "lan-mouse";
    path = lib.cleanSource ../.;
  };

  cargoLock.lockFile = ../Cargo.lock;

  cargoLock.outputHashes = {
    "reis-0.1.0" = "sha256-sRZqm6QdmgqfkTjEENV8erQd+0RL5z1+qjdmY18W3bA=";
  };

  # Set Environment Variables
  RUST_BACKTRACE = "full";

  meta = with lib; {
    description = "Lan Mouse is a mouse and keyboard sharing software";
    longDescription = ''
      Lan Mouse is a mouse and keyboard sharing software similar to universal-control on Apple devices. It allows for using multiple pcs with a single set of mouse and keyboard. This is also known as a Software KVM switch.
      The primary target is Wayland on Linux but Windows and MacOS and Linux on Xorg have partial support as well (see below for more details).
    '';
    mainProgram = "lan-mouse";
    platforms = platforms.all;
  };
}
