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
    in
    {
      packages = each (pkgs: {
        default = pkgs.callPackage ./nix/package.nix { };
      });

      checks = each (
        pkgs:
        import ./nix/checks.nix {
          inherit pkgs;
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
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
