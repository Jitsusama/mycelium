# Scripts

Programs run beside the repository rather than built into the command:

- `merge-gate.sh`: whether a head may merge into main. It must hold main as
  it is now, and nix-offload's keeper must have kept every Linux check as
  passed at that very commit. A merge tool runs main's own copy, so a branch
  can't loosen the gate it's judged by.
- `merge-train.sh`: several reviewed heads merged onto main as one, on one
  run of the checks, each named by its full sha. `make` lays them onto
  main as merge commits and pushes the tip as `train/<sha>`; `land` moves
  main to that tip once main's own gate lets it, which only goes if main
  hasn't moved since. Run main's copy, as the gate is run.
- `merge-gate-test.sh`: the gate's cases and the train's, each in a
  repository made for it, which `nix/checks.nix` runs as
  `merge-gate-holds`

## Who Makes a Train

A change's author makes and lands its own train once its reviewer has
approved the head it names, rather than waiting for the reviewer to
make one. Of the six changes landed in the day to 21 November 2026,
the median waited 18 hours from ready to landed, though a train's
checks took 19 minutes; the rest was waiting for the reviewer's next
train.

1. Take the approved head's full sha from the approval, never from the
   branch, which may have moved since. A head with no approval at that
   very sha doesn't ride.
2. Any other change approved at its current head may ride too, named by
   its full sha after yours, so changes ready together share one run of
   the checks.
3. Run main's `make`, build every Linux check at the tip it names, and
   run main's `land` on that tip.
4. Say on each rider's change that it landed, naming main's new sha.

A landing never waits for a Mac's build. Its Validation says no Mac
built it, and the Mac build runs before core.nix takes it.

Where another train lands first, `land` refuses, since main moved. Make
the train again on the new main with the same shas: `make` leaves off
any head main already holds, and any that no longer merges cleanly,
saying which. Build the checks at the new tip and land it. A head left
off for a clash goes back to its author to rebase, and needs a fresh
approval at its new head before it rides.
