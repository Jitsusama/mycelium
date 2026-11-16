# Scripts

Programs run beside the repository rather than built into the command:

- `merge-gate.sh`: whether a head may merge into main. It must hold main as
  it is now, and nix-offload's keeper must have kept every Linux check as
  passed at that very commit. A merge tool runs main's own copy, so a branch
  can't loosen the gate it's judged by.
- `merge-gate-test.sh`: the gate's cases, each in a repository made for it,
  which `nix/checks.nix` runs as `merge-gate-holds`
