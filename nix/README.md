# Nix

How the repository is built and checked, read by `flake.nix`:

- `package.nix`: the `mycelium` command, its tests run as it builds
- `checks.nix`: every check `nix flake check` runs
- `tree.sh`: the tree's own rules, which `checks.nix` runs

Nothing else goes here: the node's code is in `src/`.
