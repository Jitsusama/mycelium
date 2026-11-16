#!/usr/bin/env bash
# The merge gate's cases, each in a repository of its own made here: a
# head that holds main merges once every Linux check passed at it, and one
# that doesn't hold main, or lacks a check there, is refused, saying why.
# Then the merge train's: heads merged onto main as one, landing only as
# the gate lets the train's tip.
#
#   merge-gate-test.sh GATE TRAIN
set -euo pipefail

gate=$(realpath "${1:?Name the gate: merge-gate-test.sh GATE TRAIN}")
train=$(realpath "${2:?Name the train: merge-gate-test.sh GATE TRAIN}")
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=gate GIT_AUTHOR_EMAIL=gate@example.org
export GIT_COMMITTER_NAME=gate GIT_COMMITTER_EMAIL=gate@example.org
export MERGE_GATE_PASSED="${work}/passed"
mkdir "${MERGE_GATE_PASSED}"
failed=0

git init -q --bare -b main "${work}/origin.git"
git clone -q "${work}/origin.git" "${work}/tree" 2> /dev/null
cd "${work}/tree"
commit() {
  echo "$1" >> said
  git add said
  git commit -q -m "$1"
  git rev-parse HEAD
}
first=$(commit first)
git push -q origin HEAD:main
head=$(commit head)

# Every Linux check kept as passed at a commit, but the ones named.
pass() {
  local sha=$1
  shift
  local check
  for check in agents-cargo-goes-through-nix-offload markdown-is-clean merge-gate-holds \
    mycelium mycelium-needs-no-libraries mycelium-passes-clippy names-stay-out \
    nix-is-clean rust-is-formatted toml-is-clean tree-keeps-its-rules; do
    if [[ " $* " != *" ${check} "* ]]; then
      printf '{"check":"checks.x86_64-linux.%s","dir":null}\n' "${check}"
    fi
  done > "${MERGE_GATE_PASSED}/${sha}"
}

# Whether the gate let `sha` merge (`merges`) or refused it (`refuses`),
# and said `words`.
expect() {
  local meant=$1 sha=$2 words=$3 why=$4 said code=0
  said=$(bash "${gate}" "${sha}" 2>&1) || code=$?
  local did=merges
  ((code == 0)) || did=refuses
  if [[ ${did} != "${meant}" || ${said} != *"${words}"* ]]; then
    echo "FAIL ${why}: it ${did}, saying: ${said}" >&2
    failed=1
  fi
}

pass "${head}"
expect merges "${head}" "No Mac built it" \
  "a head on main with every check passed there merges"
echo '{"check":"checks.aarch64-darwin.mycelium","dir":null}' \
  >> "${MERGE_GATE_PASSED}/${head}"
expect merges "${head}" "a Mac built it" "a Mac's build is said"

pass "${head}" mycelium-needs-no-libraries
expect refuses "${head}" "checks.x86_64-linux.mycelium-needs-no-libraries" \
  "a head lacking one check passed there is refused, naming it"
rm "${MERGE_GATE_PASSED}/${head}"
expect refuses "${head}" "checks.x86_64-linux.mycelium " \
  "a head nothing passed at is refused"
pass "${first}"
expect refuses "${head}" "nothing kept says" \
  "what passed at another commit doesn't count"

# Main moves on past the head, as another change merges first.
git checkout -q -b other "${first}"
moved=$(commit moved)
git push -q origin HEAD:main
pass "${head}"
expect refuses "${head}" "doesn't hold main as it is now, ${moved:0:12}" \
  "a head that doesn't hold main is refused, however its checks went"

# Rebased onto main and checked again there, it merges.
git checkout -q -b rebased "${moved}"
again=$(commit head)
pass "${again}"
expect merges "${again}" "holds main" "a head rebased onto main merges"

# The train, from main as it is now. Each rider is a branch of its own off
# main, touching a file of its own, as two reviewed changes would.
export MERGE_TRAIN_GATE="${gate}"
base=$(git rev-parse origin/main)
rider() {
  git checkout -q -b "$1" "${base}"
  echo "$1" > "$1"
  git add "$1"
  git commit -q -m "$1"
  git rev-parse HEAD
}
north=$(rider north)
south=$(rider south)
# One that changes what north changes, so it can't ride behind it.
git checkout -q -b clash "${base}"
echo clash > north
git add north
git commit -q -m clash
clash=$(git rev-parse HEAD)

# What `train` said and did, as `expect` judges the gate.
rode() {
  local meant=$1 words=$2 why=$3
  shift 3
  local code=0
  said=$(bash "${train}" "$@" 2>&1) || code=$?
  local did=goes
  ((code == 0)) || did=refuses
  if [[ ${did} != "${meant}" || ${said} != *"${words}"* ]]; then
    echo "FAIL ${why}: it ${did}, saying: ${said}" >&2
    failed=1
  fi
}

rode goes "left off" "a head that clashes with one before it is left off" \
  make "${north}" "${clash}" "${south}"
tip=${said##*$'\n'}
if ! git cat-file -e "${tip}^{commit}" 2> /dev/null; then
  echo "FAIL making a train says its tip last, not: ${said}" >&2
  exit 1
fi
parents=$(git rev-parse "${tip}^1^1" "${tip}^1^2" "${tip}^2")
if [[ ${parents} != "${base}"$'\n'"${north}"$'\n'"${south}" ]]; then
  echo "FAIL the train is main's merge of north, then of south: ${parents}" >&2
  failed=1
fi
held=$(git show "${tip}:north" "${tip}:south")
if [[ ${held} != north$'\n'south ]]; then
  echo "FAIL the train's tip holds every rider's change: ${held}" >&2
  failed=1
fi
pushed=$(git ls-remote origin "refs/heads/train/${tip:0:12}")
if [[ ${pushed} != "${tip}"* ]]; then
  echo "FAIL the train's tip is pushed for a builder to fetch" >&2
  failed=1
fi

rode refuses "nothing kept says" "a train nothing passed at doesn't land" land "${tip}"
still=$(git ls-remote origin refs/heads/main)
if [[ ${still} != "${base}"* ]]; then
  echo "FAIL a train refused leaves main where it was: ${still}" >&2
  failed=1
fi

pass "${tip}"
rode goes "main is now ${tip:0:12}" "a train every check passed at lands" land "${tip}"
landed=$(git ls-remote origin refs/heads/main)
if [[ ${landed} != "${tip}"* ]]; then
  echo "FAIL main moves to the train's tip: ${landed}" >&2
  failed=1
fi
left=$(git ls-remote origin "refs/heads/train/${tip:0:12}")
if [[ -n ${left} ]]; then
  echo "FAIL a landed train's branch goes: ${left}" >&2
  failed=1
fi

rode refuses "main already holds" "a train of heads main holds has no riders" \
  make "${north}" "${south}"

# Main moves on while a train's checks run: it no longer lands.
git fetch -q origin
git checkout -q -b east "${tip}"
east=$(commit east)
rode goes "${east:0:12}" "a train of one is made" make "${east}"
late=${said##*$'\n'}
if ! git cat-file -e "${late}^{commit}" 2> /dev/null; then
  echo "FAIL making a train of one says its tip last, not: ${said}" >&2
  exit 1
fi
pass "${late}"
git checkout -q -b west "${tip}"
commit west > /dev/null
git push -q origin HEAD:main
rode refuses "doesn't hold main" "a train main moved past doesn't land" land "${late}"

exit "${failed}"
