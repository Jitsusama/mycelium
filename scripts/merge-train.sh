#!/usr/bin/env bash
# The merge train: heads whose review said yes merge into main as one, on
# one run of the Linux checks over main and every rider together. On 16
# November 2026 each merge sent every other open change back to rebase and
# build its checks again, about 20 minutes each, so five changes ready at
# once took over an hour and a half to land.
#
#   merge-train.sh make SHA...
#   merge-train.sh land TIP
#
# make lays each head, in the order named, onto main as GitHub's merge
# button would: a merge commit, with main, then each rider before it, as
# its first parent. A head main already holds, or one that doesn't merge
# cleanly onto those before it, is left off, saying why, and rides alone
# once rebased. The tip is pushed as train/<its first twelve>, so a builder
# can fetch it, and said on the last line. Build the checks there:
#
#   nix-offload there github:Jitsusama/mycelium/TIP checks.x86_64-linux...
#
# land asks main's own merge gate whether the tip may merge (it holds main
# and every check passed at it), then moves main to it, which goes only if
# main hasn't moved since, since nothing here forces a push. Each rider's
# head is then in main, so its change reads merged, and the train's branch
# goes. Where main moved first, make the train again on it.
#
# Run main's own copy, as the gate is run, so a branch can't loosen it:
#
#   git fetch -q origin main &&
#     git show origin/main:scripts/merge-train.sh | bash -s -- make SHA...
#
# MERGE_TRAIN_GATE names another gate file, for this script's tests.
set -euo pipefail

usage="merge-train.sh make SHA... | merge-train.sh land TIP"
verb=${1:?${usage}}
shift

# A commit by its sha, fetched where this checkout hasn't seen it.
known() {
  if ! git cat-file -e "$1^{commit}" 2> /dev/null; then
    git fetch -q origin "$1"
  fi
  git rev-parse "$1^{commit}"
}

make_train() {
  (($# > 0)) || {
    echo "merge-train: name the heads to ride: ${usage}" >&2
    exit 1
  }
  git fetch -q origin main
  local main tip riders=0 head sha tree
  main=$(git rev-parse origin/main)
  tip=${main}
  for head in "$@"; do
    sha=$(known "${head}")
    if git merge-base --is-ancestor "${sha}" "${main}"; then
      echo "merge-train: ${sha:0:12} is left off: main already holds it." >&2
      continue
    fi
    if ! tree=$(git merge-tree --write-tree --no-messages "${tip}" "${sha}" 2> /dev/null); then
      echo "merge-train: ${sha:0:12} is left off: it doesn't merge cleanly onto" \
        "main and the riders before it, so rebase it and let it ride alone." >&2
      continue
    fi
    tree=${tree%%$'\n'*}
    tip=$(git log -1 --format=%s "${sha}" |
      git commit-tree "${tree}" -p "${tip}" -p "${sha}" \
        -m "Merge ${sha:0:12} on the merge train" -F -)
    riders=$((riders + 1))
    echo "merge-train: ${sha:0:12} rides." >&2
  done
  if ((riders == 0)); then
    echo "merge-train: nothing rides, so there's no train to make." >&2
    exit 1
  fi
  git push -q origin "${tip}:refs/heads/train/${tip:0:12}"
  echo "merge-train: the train is ${tip:0:12} on main ${main:0:12}, ${riders}" \
    "riding, pushed as train/${tip:0:12}. Build the checks there, then land it."
  echo "${tip}"
}

land_train() {
  local tip
  tip=$(known "${1:?Name the tip of the train: ${usage}}")
  git fetch -q origin main
  if [[ -n ${MERGE_TRAIN_GATE:-} ]]; then
    bash "${MERGE_TRAIN_GATE}" "${tip}"
  else
    git show origin/main:scripts/merge-gate.sh | bash -s -- "${tip}"
  fi
  git push -q origin "${tip}:refs/heads/main"
  git push -q origin --delete "train/${tip:0:12}" 2> /dev/null || true
  echo "merge-train: main is now ${tip:0:12}, every rider in it."
}

case ${verb} in
  make) make_train "$@" ;;
  land) land_train "$@" ;;
  *)
    echo "merge-train: no verb ${verb}: ${usage}" >&2
    exit 1
    ;;
esac
