# Mycelium's package, its development shell and its one gate: `nix flake
# check` runs every check there is, on Linux and macOS alike.
{
  description = "A nimble core for agent networks";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      each = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # The command linked in whole against musl, so it runs on a Linux machine
      # with no Nix, such as a provider's sandbox. Where no musl is named for a
      # system, there is none.
      musl = {
        x86_64-linux = "musl64";
        aarch64-linux = "aarch64-multiplatform-musl";
      };
      static =
        pkgs:
        (pkgs.pkgsCross.${musl.${pkgs.stdenv.hostPlatform.system}}.callPackage ./nix/package.nix { })
        .overrideAttrs
          (old: {
            env = (old.env or { }) // {
              RUSTFLAGS = "-C target-feature=+crt-static";
            };
          });
      hasStatic = pkgs: musl ? ${pkgs.stdenv.hostPlatform.system};
    in
    {
      packages = each (
        pkgs:
        {
          default = pkgs.callPackage ./nix/package.nix { };
        }
        // nixpkgs.lib.optionalAttrs (hasStatic pkgs) { static = static pkgs; }
      );

      checks = each (
        pkgs:
        import ./nix/checks.nix {
          inherit pkgs;
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          static = self.packages.${pkgs.stdenv.hostPlatform.system}.static or null;
          source = ./.;
        }
      );

      devShells = each (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = [
            pkgs.clippy
            pkgs.deadnix
            pkgs.nixfmt
            pkgs.rumdl
            pkgs.rust-analyzer
            pkgs.rustfmt
            pkgs.shellcheck
            pkgs.statix
            pkgs.taplo
          ];
        };
      });

      formatter = each (pkgs: pkgs.nixfmt);
    };
}
