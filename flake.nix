{
  description = "Nix Flake for lan-mouse";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    systems.url = "github:nix-systems/default";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    {
      nixpkgs,
      systems,
      rust-overlay,
      self,
      ...
    }:
    let
      inherit (nixpkgs) lib;
      forEachPkgs =
        f:
        lib.genAttrs (import systems) (
          system:
          let
            pkgs = import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            };
            # Default toolchain for devshell
            rustToolchain = pkgs.rust-bin.stable.latest.default.override {
              extensions = [
                # includes already:
                # rustc
                # cargo
                # rust-std
                # rust-docs
                # rustfmt-preview
                # clippy-preview
                "rust-analyzer"
                "rust-src"
              ];
            };
            # Minimal toolchain for builds (rustc + cargo + rust-std only)
            rustToolchainForBuild = pkgs.rust-bin.stable.latest.minimal;
          in
          f { inherit pkgs rustToolchain rustToolchainForBuild; }
        );
    in
    {
      packages = forEachPkgs (
        { pkgs, rustToolchainForBuild, ... }:
        let
          customRustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchainForBuild;
            rustc = rustToolchainForBuild;
          };
          lan-mouse = pkgs.callPackage ./nix { rustPlatform = customRustPlatform; };
        in
        {
          default = lan-mouse;
          inherit lan-mouse;
        }
      );
      devShells = forEachPkgs (
        { pkgs, rustToolchain, ... }:
        {
          default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                rustToolchain
                pkg-config
                gtk4
                libadwaita
                librsvg
              ]
              ++ lib.optionals pkgs.stdenv.isLinux [
                libX11
                libXtst
              ];
            RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          };
        }
      );
      homeManagerModules.default = import ./nix/hm-module.nix self;
    };
}
