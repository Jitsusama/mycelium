# Nix

How the repository is built and checked, read by `flake.nix`:

- `package.nix`: the `mycelium` command, its tests run as it builds
- `checks.nix`: every check `nix flake check` runs
- `tree.sh`: the tree's own rules, which `checks.nix` runs
- `cargo-for-agents.nix` and `cargo-for-agents.sh`: the dev shell's cargo,
  which sends an agent's whole suite to a lent builder through
  `nix-offload`, and `cargo-for-agents-test.sh`, its cases

Nothing else goes here: the node's code is in `src/`.
