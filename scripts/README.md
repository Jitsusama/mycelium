# Scripts

Programs run beside the repository rather than built into the command:

- `merge-gate.sh`: whether a head may merge into main. It must hold main as
  it is now, and nix-offload's keeper must have kept every Linux check as
  passed at that very commit. A merge tool runs main's own copy, so a branch
  can't loosen the gate it's judged by. It also runs main's
  `nix/counts.sh` over the head, and refuses a head whose lists of what
  waits hold a line or a count main's don't. The one rise it lets through
  is by a sharper reader the head brings: in a regular Rust file the head
  leaves as main has it, by the lines that reader names in its check
  `counts-name-their-lines` and main's doesn't count, each counted once.
  As the head's kept `counts-only-fall` holds its reader's count to its
  list, that holds for any reader whose names agree with its count, so
  it only checks that the lines it names beyond main's cover the rise.
  What that reader counts is bounded by its review, as any change to the
  rule is: of its `nix/counts.sh`, of `counts-name-their-lines` and
  `counts-only-fall` in its `nix/checks.nix`, and of whatever its
  `flake.nix` and `flake.lock` make of them. To judge a rise, the gate
  builds that check from the head's flake with the nix on the machine it
  runs on, sandboxed as that machine's settings sandbox a build, within
  `MERGE_GATE_BUILD_SECONDS`, 1800 seconds unless set, and needs
  coreutils' `timeout` to bound it.
- `merge-train.sh`: several reviewed heads merged onto main as one, on one
  run of the checks, each named by its full sha. `make` lays them onto
  main as merge commits and pushes the tip as `train/<sha>`; `land` moves
  main to that tip once main's own gate lets it, which only goes if main
  hasn't moved since. Run main's copy, as the gate is run.
- `merge-gate-test.sh`: the gate's cases and the train's, each in a
  repository made for it, which `nix/checks.nix` runs as
  `merge-gate-holds`
- `merge-gate-rise-test.sh`: the gate's cases for a rise in what waits,
  with a dull stand-in for main's reader and for nix, run beside them
- `gate-test-kit.sh`: what the two share, sourced by each: their folder,
  git's settings, the checks kept as passed, and `expect`
