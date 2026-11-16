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
# Each SHA is a reviewed head's full commit id, never a branch, so what
# rides is what was reviewed even if its branch moves on.
#
# make lays each head, in the order named, onto main as GitHub's merge
# button would: a merge commit, with main, then each rider before it, as
# its first parent. A head main or the train already holds, or one that
# doesn't merge cleanly onto those before it, is left off, saying why. Its
# commits are dated as main's tip is, so making the same train again on
# the same main gives the same tip, and the checks already built there
# still count. The tip is pushed as train/<its first twelve>, so a builder
# can fetch it, and said on the last line. Build every check the gate asks
# for there:
#
#   nix-offload there github:Jitsusama/mycelium/TIP checks.x86_64-linux.mycelium ...
#
# land asks main's own merge gate whether the tip may merge (it holds main
# and every check passed at it), then moves main to it without forcing, so
# it goes only if main hasn't moved since. Each rider's head is then in
# main, and its change reads merged so long as that is still its head. The
# train's branch goes. Where main moved first, make the train again on it.
#
# Run main's own copy, as the gate is run, so no branch can loosen it:
#
#   git fetch -q origin main &&
#     git show origin/main:scripts/merge-train.sh | bash -s -- make SHA...
set -euo pipefail

usage="merge-train.sh make SHA... | merge-train.sh land TIP"
verb=${1:?${usage}}
shift

# A reviewed commit by its full id, fetched where this checkout hasn't
# seen it. A branch or a short id could name what nobody reviewed.
known() {
  if [[ ! $1 =~ ^[0-9a-f]{40}$ ]]; then
    echo "merge-train: ${1@Q} isn't a commit's full id; name the reviewed" \
      "head's sha whole." >&2
    exit 1
  fi
  if ! git cat-file -e "$1^{commit}" 2> /dev/null; then
    git fetch -q origin "$1"
  fi
  echo "$1"
}

# The tree `tip` and `sha` merge to, or nothing where they clash. Any other
# failure of git's is said as it is and ends the train.
merged() {
  local tree code=0
  tree=$(git merge-tree --write-tree --no-messages "$1" "$2") || code=$?
  case ${code} in
    0) echo "${tree%%$'\n'*}" ;;
    1) ;;
    *)
      echo "merge-train: git couldn't merge ${2:0:12} (exit ${code}); it needs" \
        "git 2.38 or later." >&2
      exit 1
      ;;
  esac
}

make_train() {
  (($# > 0)) || {
    echo "merge-train: name the heads to ride: ${usage}" >&2
    exit 1
  }
  git fetch -q origin main
  local main tip head sha tree onto
  local -a riding=() off=()
  main=$(git rev-parse origin/main)
  tip=${main}
  # Dated as main is, so the same train made again is the same commits.
  GIT_AUTHOR_DATE=$(git log -1 --format=%cI "${main}")
  export GIT_AUTHOR_DATE GIT_COMMITTER_DATE=${GIT_AUTHOR_DATE}
  for head in "$@"; do
    sha=$(known "${head}")
    if git merge-base --is-ancestor "${sha}" "${tip}"; then
      off+=("${sha:0:12}, which main or a rider before it already holds")
      continue
    fi
    tree=$(merged "${tip}" "${sha}")
    if [[ -z ${tree} ]]; then
      onto=$(merged "${main}" "${sha}")
      if [[ -z ${onto} ]]; then
        off+=("${sha:0:12}, which clashes with main: rebase it")
      else
        off+=("${sha:0:12}, which clashes with a rider before it: it rides after them")
      fi
      continue
    fi
    tip=$(git log -1 --format=%s "${sha}" |
      git commit-tree "${tree}" -p "${tip}" -p "${sha}" \
        -m "Merge ${sha:0:12} on the merge train" -F -)
    riding+=("${sha:0:12}")
  done
  local each
  for each in "${off[@]}"; do
    echo "merge-train: left off ${each}." >&2
  done
  if ((${#riding[@]} == 0)); then
    echo "merge-train: nothing rides, so there's no train to make." >&2
    exit 1
  fi
  sweep "${main}"
  git push -q origin "${tip}:refs/heads/train/${tip:0:12}"
  echo "merge-train: the train is ${tip:0:12} on main ${main:0:12}, riding" \
    "${riding[*]}; ${#off[@]} left off. It's pushed as train/${tip:0:12}:" \
    "build the checks there, then land it."
  echo "${tip}"
}

# Every train's branch whose tip no longer holds `main`, which can never
# land, goes. One this checkout hasn't seen is fetched first, so a train
# another made on main is kept.
sweep() {
  local ref sha trains
  trains=$(git ls-remote origin 'refs/heads/train/*')
  while read -r sha ref; do
    [[ -n ${sha} ]] || continue
    git cat-file -e "${sha}^{commit}" 2> /dev/null || git fetch -q origin "${sha}"
    if ! git merge-base --is-ancestor "$1" "${sha}"; then
      git push -q origin --delete "${ref#refs/heads/}" ||
        echo "merge-train: couldn't delete the spent ${ref#refs/heads/}." >&2
    fi
  done <<< "${trains}"
}

land_train() {
  local tip
  tip=$(known "${1:?Name the tip of the train: ${usage}}")
  git fetch -q origin main
  git show origin/main:scripts/merge-gate.sh | bash -s -- "${tip}"
  if ! git push -q origin "${tip}:refs/heads/main"; then
    echo "merge-train: main moved while the gate judged ${tip:0:12}, so it" \
      "didn't land: make the train again on main." >&2
    exit 1
  fi
  git push -q origin --delete "train/${tip:0:12}" ||
    echo "merge-train: couldn't delete train/${tip:0:12}; it's spent." >&2
  echo "merge-train: main is now ${tip:0:12}, each rider's head in it as made."
}

case ${verb} in
  make) make_train "$@" ;;
  land) land_train "$@" ;;
  *)
    echo "merge-train: no verb ${verb}: ${usage}" >&2
    exit 1
    ;;
esac
