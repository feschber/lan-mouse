# Nix Flake Usage

## Run

```bash
nix run github:feschber/lan-mouse

# With params
nix run github:feschber/lan-mouse -- --help

```

## Home-manager module

Add input:

```nix
inputs = {
    lan-mouse.url = "github:feschber/lan-mouse";
}
```

Optional: add [our binary cache](https://app.cachix.org/cache/lan-mouse) to allow a faster package install.

```nix
nixConfig = {
    extra-substituters = [
        "https://lan-mouse.cachix.org/"
    ];
    extra-trusted-public-keys = [
      "lan-mouse.cachix.org-1:KlE2AEZUgkzNKM7BIzMQo8w9yJYqUpor1CAUNRY6OyM="
    ];
};
```

Enable lan-mouse:

``` nix
{
  inputs,
  ...
}: {
  # Add the Home Manager module
  imports = [inputs.lan-mouse.homeManagerModules.default];

  programs.lan-mouse = {
    enable = true;
    # systemd = false;
    # package = inputs.lan-mouse.packages.${pkgs.stdenv.hostPlatform.system}.default
    # Optional configuration in nix syntax, see config.toml for available options
    # settings = { };
    };
  };
}

```
