# shellcheck shell=bash
# What the merge gate's tests share, sourced once each has named ${gate}:
# a folder of their own for the repositories they make, a git that reads
# no one's settings, the checks kept as passed at a commit, whether the
# gate merged or refused one, saying what, and an end saying if any case
# failed.

: "${gate:?Name the gate before sourcing gate-test-kit.sh}"
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=gate GIT_AUTHOR_EMAIL=gate@example.org
export GIT_COMMITTER_NAME=gate GIT_COMMITTER_EMAIL=gate@example.org
export MERGE_GATE_PASSED="${work}/passed"
mkdir "${MERGE_GATE_PASSED}"
failed=0

# A commit of what's staged here, its words added to a file of its own so
# no two are alike, said as its sha.
commit() {
  echo "$1" >> said
  git add said
  git commit -q -m "$1"
  git rev-parse HEAD
}

# Every Linux check kept as passed at a commit, but the ones named.
pass() {
  local sha=$1
  shift
  local check
  for check in agents-cargo-goes-through-nix-offload counts-name-their-lines \
    counts-only-fall markdown-is-clean merge-gate-holds \
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

# Ends the test, failing if any case did.
finish() {
  exit "${failed}"
}
