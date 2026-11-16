#!/usr/bin/env bash
# The merge gate's cases, each in a repository of its own made here: a
# head that holds main merges once every Linux check passed at it, and one
# that doesn't hold main, or lacks a check there, is refused, saying why.
#
#   merge-gate-test.sh GATE
set -euo pipefail

gate=$(realpath "${1:?Name the gate: merge-gate-test.sh GATE}")
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

exit "${failed}"
