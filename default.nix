{ pkgs ? import <nixpkgs> { }
}:
pkgs.callPackage nix/default.nix { }
