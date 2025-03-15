self: {
  config,
  pkgs,
  lib,
  ...
}:
with lib; let
  cfg = config.programs.lan-mouse;
  defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  tomlFormat = pkgs.formats.toml {};
in {
  options.programs.lan-mouse = with types; {
    enable = mkEnableOption "Whether or not to enable lan-mouse.";
    package = mkOption {
      type = with types; nullOr package;
      default = defaultPackage;
      defaultText = literalExpression "inputs.lan-mouse.packages.${pkgs.stdenv.hostPlatform.system}.default";
      description = ''
        The lan-mouse package to use.

        By default, this option will use the `packages.default` as exposed by this flake.
      '';
    };
    systemd = mkOption {
      type = types.bool;
      default = pkgs.stdenv.isLinux;
      description = "Whether to enable to systemd service for lan-mouse on linux.";
    };
    launchd = mkOption {
      type = types.bool;
      default = pkgs.stdenv.isDarwin;
      description = "Whether to enable to launchd service for lan-mouse on macOS.";
    };
    settings = lib.mkOption {
      inherit (tomlFormat) type;
      default = {};
      example = builtins.fromTOML (builtins.readFile (self + /config.toml));
      description = ''
        Optional configuration written to {file}`$XDG_CONFIG_HOME/lan-mouse/config.toml`.

        See <https://github.com/feschber/lan-mouse/> for
        available options and documentation.
      '';
    };
  };

  config = mkIf cfg.enable {
    systemd.user.services.lan-mouse = lib.mkIf cfg.systemd {
      Unit = {
        Description = "Systemd service for Lan Mouse";
        Requires = ["graphical-session.target"];
      };
      Service = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/lan-mouse daemon";
      };
      Install.WantedBy = [
        (lib.mkIf config.wayland.windowManager.hyprland.systemd.enable "hyprland-session.target")
        (lib.mkIf config.wayland.windowManager.sway.systemd.enable "sway-session.target")
      ];
    };

    launchd.agents.lan-mouse = lib.mkIf cfg.launchd {
      enable = true;
      config = {
        ProgramArguments = [
          "${cfg.package}/bin/lan-mouse"
          "daemon"
        ];
        KeepAlive = true;
      };
    };

    home.packages = [
      cfg.package
    ];

    xdg.configFile."lan-mouse/config.toml" = lib.mkIf (cfg.settings != {}) {
      source = tomlFormat.generate "config.toml" cfg.settings;
    };
  };
}
