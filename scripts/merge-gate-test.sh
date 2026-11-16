#!/usr/bin/env bash
# The merge gate's cases, each in a repository of its own made here: a
# head that holds main merges once every Linux check passed at it, and one
# that doesn't hold main, or lacks a check there, is refused, saying why.
# Then the merge train's: heads merged onto main as one, landing only as
# main's own gate lets the train's tip.
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

# The tip a train just made said, or the test ends.
made() {
  local tip=${said##*$'\n'}
  if ! git cat-file -e "${tip}^{commit}" 2> /dev/null; then
    echo "FAIL making a train says its tip last, not: ${said}" >&2
    exit 1
  fi
  echo "${tip}"
}

# Whether origin's `ref` points at `want`, or at nothing where `want` is
# empty, as `why` says it should.
points() {
  local ref=$1 want=$2 why=$3 line
  line=$(git ls-remote origin "${ref}")
  if [[ ${line%%$'\t'*} != "${want}" ]]; then
    echo "FAIL ${why}: ${ref} is at ${line:-nothing}" >&2
    failed=1
  fi
}

rode refuses "name the heads" "a train of nothing isn't made" make
rode refuses "no verb" "a verb the train doesn't know is refused" leave
rode refuses "full id" "a short id is refused, so only what was reviewed rides" \
  make "${again:0:12}"
rode refuses "full id" "a branch's name is refused the same" make rebased

# The train lands as main's own gate says, so main carries the gate, as
# mycelium's does; a branch can't bring a looser one.
git checkout -q -b gated "${moved}"
mkdir -p scripts
cp "${gate}" scripts/merge-gate.sh
git add scripts/merge-gate.sh
git commit -q -m "the gate"
git push -q origin HEAD:main
base=$(git rev-parse HEAD)

# Each rider is a branch of its own off main, touching a file of its own,
# as two reviewed changes would.
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
# One that changes what main changed since it branched, so it must rebase.
git checkout -q -b stale "${first}"
echo stale > said
git add said
git commit -q -m stale
stale=$(git rev-parse HEAD)
# One made in another clone and pushed only to origin, as a peer's is.
git clone -q "${work}/origin.git" "${work}/peer" 2> /dev/null
git -C "${work}/peer" checkout -q -b peer origin/main
echo peer > "${work}/peer/peer"
git -C "${work}/peer" add peer
git -C "${work}/peer" commit -q -m peer
peer=$(git -C "${work}/peer" rev-parse HEAD)
git -C "${work}/peer" push -q origin HEAD:peer

rode goes "${clash:0:12}, which clashes with a rider before it" \
  "a head that clashes with one before it is left off, saying so" \
  make "${north}" "${clash}" "${stale}" "${south}" "${north}" "${peer}"
if [[ ${said} != *"${stale:0:12}, which clashes with main: rebase it"* ]]; then
  echo "FAIL a head that clashes with main is told to rebase: ${said}" >&2
  failed=1
fi
if [[ ${said} != *"riding ${north:0:12} ${south:0:12} ${peer:0:12}; 3 left off"* ]]; then
  echo "FAIL the train names each rider and how many were left off: ${said}" >&2
  failed=1
fi
tip=$(made)
parents=$(git rev-parse "${tip}^1^1^1" "${tip}^1^1^2" "${tip}^1^2" "${tip}^2")
if [[ ${parents} != "${base}"$'\n'"${north}"$'\n'"${south}"$'\n'"${peer}" ]]; then
  echo "FAIL the train is main's merge of north, then south, then peer's:" \
    "${parents}" >&2
  failed=1
fi
held=$(git show "${tip}:north" "${tip}:south" "${tip}:peer")
if [[ ${held} != north$'\n'south$'\n'peer ]]; then
  echo "FAIL the train's tip holds every rider's change: ${held}" >&2
  failed=1
fi
points "refs/heads/train/${tip:0:12}" "${tip}" \
  "the train's tip is pushed for a builder to fetch"
rode goes "" "the same train made again" make "${north}" "${south}" "${peer}"
again=$(made)
if [[ ${again} != "${tip}" ]]; then
  echo "FAIL the same train made again on the same main is the same tip," \
    "so its checks still count: ${again} against ${tip}" >&2
  failed=1
fi

rode refuses "nothing kept says" "a train nothing passed at doesn't land" land "${tip}"
points refs/heads/main "${base}" "a train refused leaves main where it was"
points "refs/heads/train/${tip:0:12}" "${tip}" \
  "a train refused keeps its branch, to build and land again"

pass "${tip}"
rode goes "main is now ${tip:0:12}" "a train every check passed at lands" land "${tip}"
points refs/heads/main "${tip}" "main moves to the train's tip"
points "refs/heads/train/${tip:0:12}" "" "a landed train's branch goes"

rode refuses "main or a rider before it already holds" \
  "a train of heads main holds has no riders" make "${north}" "${south}"

# Main moves on while a train's checks run: it no longer lands.
git fetch -q origin
git checkout -q -b east "${tip}"
echo east > east
git add east
git commit -q -m east
east=$(git rev-parse HEAD)
rode goes "riding ${east:0:12}" "a train of one is made" make "${east}"
late=$(made)
pass "${late}"
git checkout -q -b west "${tip}"
west=$(commit west)
git push -q origin HEAD:main
rode refuses "doesn't hold main" "a train main moved past doesn't land" land "${late}"
rode goes "riding ${east:0:12}" "making it again on the new main" make "${east}"
points "refs/heads/train/${late:0:12}" "" \
  "a train that can't land any more is swept away"

# Main moves while the gate judges: the push that would land refuses
# rather than force, and main keeps what came first. Main's gate here
# lets the tip through, then lands another change before the train can.
git checkout -q -b racing "${west}"
cat > scripts/merge-gate.sh << 'GATE'
#!/usr/bin/env bash
set -euo pipefail
bash "${RACE_GATE}" "$1"
git push -q origin "${RACE_FIRST}:refs/heads/main"
GATE
git add scripts/merge-gate.sh
git commit -q -m "a gate that races"
racing=$(git rev-parse HEAD)
git push -q origin HEAD:main
interloper=$(commit interloper)
export RACE_GATE=${gate} RACE_FIRST=${interloper}
git checkout -q -b race "${racing}"
echo race > race
git add race
git commit -q -m race
race=$(git rev-parse HEAD)
rode goes "riding ${race:0:12}" "a train is made on the racing main" make "${race}"
raced=$(made)
pass "${raced}"
rode refuses "main moved while the gate judged ${raced:0:12}" \
  "a train main moved under while its gate judged doesn't land" land "${raced}"
points refs/heads/main "${interloper}" "main keeps the change that landed first"

exit "${failed}"
