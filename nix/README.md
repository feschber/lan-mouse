# Nix Flake Usage

## run

```bash
nix run github:feschber/lan-mouse

# with params
nix run github:feschber/lan-mouse -- --help

```

## home-manager module

add input

```nix
inputs = {
    lan-mouse.url = "github:feschber/lan-mouse";
}
```

enable lan-mouse

``` nix
{
  inputs,
  ...
}: {
  # add the home manager module
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
