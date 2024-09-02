{
  description = "Nix Flake for lan-mouse";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    ...
  }: let
    inherit (nixpkgs) lib;
    genSystems = lib.genAttrs [
      "aarch64-darwin"
      "aarch64-linux"
      "x86_64-darwin"
      "x86_64-linux"
    ];
    pkgsFor = system:
      import nixpkgs {
        inherit system;

        overlays = [
          rust-overlay.overlays.default
        ];
      };
    mkRustToolchain = pkgs:
      pkgs.rust-bin.stable.latest.default.override {
        extensions = ["rust-src"];
      };
    pkgs = genSystems (system: import nixpkgs {inherit system;});
  in {
    packages = genSystems (system: rec {
      default = pkgs.${system}.callPackage ./nix {};
      lan-mouse = default;
    });
    homeManagerModules.default = import ./nix/hm-module.nix self;
    devShells = genSystems (system: let
      pkgs = pkgsFor system;
      rust = mkRustToolchain pkgs;
    in {
      default = pkgs.mkShell {
        packages = with pkgs; [
          rust
          rust-analyzer-unwrapped
          pkg-config
          xorg.libX11
          gtk4
          libadwaita
          librsvg
          xorg.libXtst
        ] ++ lib.optionals stdenv.isDarwin
        (with darwin.apple_sdk_11_0.frameworks; [
          CoreGraphics
          ApplicationServices
        ]);

        RUST_SRC_PATH = "${rust}/lib/rustlib/src/rust/library";
      };
    });
  };
}
