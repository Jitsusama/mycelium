# Scripts

Programs run beside the repository rather than built into the command:

- `merge-gate.sh`: whether a head may merge into main. It must hold main as
  it is now, and nix-offload's keeper must have kept every Linux check as
  passed at that very commit. A merge tool runs main's own copy, so a branch
  can't loosen the gate it's judged by.
- `merge-train.sh`: several reviewed heads merged onto main as one, on one
  run of the checks. `make` lays them onto main as merge commits and
  pushes the tip as `train/<sha>`; `land` moves main to that tip once the
  gate lets it, which only goes if main hasn't moved since.
- `merge-gate-test.sh`: the gate's cases and the train's, each in a
  repository made for it, which `nix/checks.nix` runs as
  `merge-gate-holds`
