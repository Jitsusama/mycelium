#!/usr/bin/env bash
# The dev shell's cargo: an agent's goes through `nix-offload cargo`, which
# sends a whole suite to a lent builder and runs the rest here, so the
# machine a person works at isn't the one compiling it. A person's cargo,
# one already running on a lent builder, and one beside no nix-offload, or
# one too old to know `cargo`, is cargo itself. It passes cargo's own words
# and answers on, so it has no help of its own.
set -euo pipefail

# Asked with no words, a nix-offload that knows `cargo` runs what it's
# named, here true; an older one refuses, as it would the real run, so a
# machine not yet updated keeps its cargo working.
if [[ -n "${PI_CODING_AGENT:-}" && -z "${WORK_RUNS_ELSEWHERE:-}" ]] &&
  command -v nix-offload >/dev/null 2>&1 &&
  nix-offload cargo --cargo @true@ -- >/dev/null 2>&1; then
  exec nix-offload cargo --cargo @cargo@ -- "$@"
fi
exec @cargo@ "$@"
