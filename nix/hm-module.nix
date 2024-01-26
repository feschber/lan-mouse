self: {
  config,
  pkgs,
  lib,
  ...
}:
with lib; let
  cfg = config.programs.lan-mouse;
  defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
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
      description = "Whether to enable to systemd service for lan-mouse.";
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
        ExecStart = "${cfg.package}/bin/lan-mouse --daemon";
      };
      Install.WantedBy = [
        (lib.mkIf config.wayland.windowManager.hyprland.systemd.enable "hyprland-session.target")
        (lib.mkIf config.wayland.windowManager.sway.systemd.enable "sway-session.target")
      ];
    };

    home.packages = [
      cfg.package
    ];
  };
}
