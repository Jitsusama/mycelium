#!/usr/bin/env bash
# Whether a head may merge into main: it holds main as it is now, and every
# Linux check passed at that very commit. On 16 November 2026 two changes
# each passed on their own base, and main stopped compiling once both were
# in, since nothing proved the head that merged was main as it would be.
#
#   merge-gate.sh SHA
#
# Run from a checkout of the repository, as a merge tool runs its gate,
# main's own copy judging, so a branch can't loosen the gate it's judged
# by:
#
#   bash -c 'git fetch -q origin main &&
#     git show origin/main:scripts/merge-gate.sh | bash -s -- "$1"' gate SHA
#
# Only what nix-offload's keeper kept as passed counts, since only the
# keeper writes it: its folder where the machine runs a keeper, else the
# account's own. MERGE_GATE_PASSED names another folder, for this gate's
# tests. A Mac's build is said, never asked: where no Mac answered, the
# merge says so, and a Mac builds it before core.nix takes it (AGENTS.md).
set -euo pipefail

sha=${1:?Name the commit to merge: merge-gate.sh SHA}
system=x86_64-linux
# Every check the flake has on Linux; nix/checks.nix fails when this list
# and the flake's part ways.
checks=(
  markdown-is-clean
  merge-gate-holds
  mycelium
  mycelium-needs-no-libraries
  mycelium-passes-clippy
  names-stay-out
  nix-is-clean
  rust-is-formatted
  toml-is-clean
  tree-keeps-its-rules
)

passed=${MERGE_GATE_PASSED:-}
if [[ -z ${passed} ]]; then
  if [[ -e /etc/systemd/system/nix-offload-keep.socket ]]; then
    passed=/var/lib/nix-offload-passed
  else
    passed=${XDG_STATE_HOME:-${HOME}/.local/state}/nix-offload/passed
  fi
fi

git fetch -q origin main
main=$(git rev-parse origin/main)
if ! git cat-file -e "${sha}^{commit}" 2> /dev/null; then
  git fetch -q origin "${sha}"
fi
if ! git merge-base --is-ancestor "${main}" "${sha}"; then
  echo "merge-gate: ${sha:0:12} doesn't hold main as it is now, ${main:0:12}," \
    "so what it merges into was never built: rebase onto main and run the" \
    "checks again." >&2
  exit 1
fi

kept=""
if [[ -r "${passed}/${sha}" ]]; then
  kept=$(< "${passed}/${sha}")
fi
missing=()
for check in "${checks[@]}"; do
  if ! grep -qF "\"check\":\"checks.${system}.${check}\"" <<< "${kept}"; then
    missing+=("checks.${system}.${check}")
  fi
done
if ((${#missing[@]} > 0)); then
  echo "merge-gate: nothing kept says these passed at ${sha:0:12}:" \
    "${missing[*]}. Build them there with nix-offload there . ${missing[*]}" >&2
  exit 1
fi

if grep -qF '"check":"checks.aarch64-darwin.mycelium"' <<< "${kept}"; then
  echo "merge-gate: ${sha:0:12} holds main, every Linux check passed there," \
    "and a Mac built it."
else
  echo "merge-gate: ${sha:0:12} holds main and every Linux check passed" \
    "there. No Mac built it: say so in the Validation, and build" \
    "checks.aarch64-darwin.mycelium before core.nix takes it."
fi
