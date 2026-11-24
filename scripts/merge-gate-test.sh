#!/usr/bin/env bash
# The merge gate's cases, each in a repository of its own made here: a
# head that holds main merges once every Linux check passed at it, and one
# that doesn't hold main, or lacks a check there, or raises a count that
# only falls, is refused, saying why. Then the merge train's: heads merged
# onto main as one, landing only as main's own gate lets the train's tip.
#
#   merge-gate-test.sh GATE TRAIN COUNTS
set -euo pipefail

usage="merge-gate-test.sh GATE TRAIN COUNTS"
gate=$(realpath "${1:?Name the gate: ${usage}}")
train=$(realpath "${2:?Name the train: ${usage}}")
counts=$(realpath "${3:?Name the counts: ${usage}}")
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
# Main holds the counts' rule and a list with one suppression waiting.
mkdir nix src
cp "${counts}" nix/counts.sh
echo '#[allow(dead_code)]' > src/old.rs
printf '%s\n' "# Waiting." "src/old.rs 1" > nix/suppressions-waiting.txt
git add nix src
first=$(commit first)
git push -q origin HEAD:main
head=$(commit head)

# Every Linux check kept as passed at a commit, but the ones named.
pass() {
  local sha=$1
  shift
  local check
  for check in agents-cargo-goes-through-nix-offload counts-only-fall markdown-is-clean \
    merge-gate-holds \
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
expect refuses "${head}" "nothing kept in ${MERGE_GATE_PASSED} says" \
  "what passed at another commit doesn't count"

# A head off main holding each `path=text` named, with every check passed
# there, so only the counts can refuse it.
counted() {
  git checkout -q -B counted "${first}"
  local pair
  for pair in "$@"; do
    mkdir -p "$(dirname "${pair%%=*}")"
    printf '%s\n' "${pair#*=}" > "${pair%%=*}"
    git add "${pair%%=*}"
  done
  git commit -q -m counted
  local sha
  sha=$(git rev-parse HEAD)
  pass "${sha}"
  echo "${sha}"
}
loose='src/loose.rs=#[allow(dead_code)]'
sha=$(counted "${loose}")
expect refuses "${sha}" "src/loose.rs: 1, listed 0" \
  "a head that turns a lint off is refused by main's counts"
sha=$(counted "${loose}" "nix/suppressions-waiting.txt=src/loose.rs 1")
expect refuses "${sha}" "src/loose.rs 1, main has 0" \
  "a head that lists its own new suppression is refused"
sha=$(counted "${loose}" "nix/counts.sh=exit 0")
expect refuses "${sha}" "src/loose.rs: 1, listed 0" \
  "a head that loosens the counts' rule is judged by main's"
# A head that takes the rule away, or leaves a link or a folder in its
# place, which merged would leave main with no rule to judge the next head
# by.
git checkout -q -B counted "${first}"
git rm -q nix/counts.sh
git commit -q -m dropped
sha=$(git rev-parse HEAD)
pass "${sha}"
expect refuses "${sha}" "doesn't keep nix/counts.sh as a file" \
  "a head that takes the counts' rule away is refused"
ln -s ../said nix/counts.sh
git add nix/counts.sh
git commit -q -m linked
sha=$(git rev-parse HEAD)
pass "${sha}"
expect refuses "${sha}" "doesn't keep nix/counts.sh as a file" \
  "a head that turns the counts' rule into a link is refused"
git rm -q nix/counts.sh
mkdir nix/counts.sh
echo 'exit 0' > nix/counts.sh/rule
git add nix/counts.sh
git commit -q -m folder
sha=$(git rev-parse HEAD)
pass "${sha}"
expect refuses "${sha}" "doesn't keep nix/counts.sh as a file" \
  "a head that puts a folder where the counts' rule was is refused"
# The gate run from a folder below the top reads the rule and the whole
# tree where they are, and the kept checks where it was told.
fine=$(counted 'src/fine.rs=fn fine() {}')
elsewhere=$(counted 'tools/loose.rs=#[allow(dead_code)]')
cd src
expect merges "${fine}" "holds main" "the gate run from a folder below the top finds the rule"
MERGE_GATE_PASSED=../../passed expect merges "${fine}" "holds main" \
  "the kept checks named from a folder below the top are read from there"
expect refuses "${elsewhere}" "tools/loose.rs: 1, listed 0" \
  "the gate run from a folder below the top counts the whole tree"
cd ..
sha=$(counted "nix/suppressions-waiting.txt=src/main.rs many")
expect refuses "${sha}" "is not a path and a count above 0" \
  "a list line whose count isn't a number is refused"
sha=$(counted "nix/suppressions-waiting.txt=src/old.rs")
expect refuses "${sha}" "is not a path and a count above 0" \
  "a suppression line with no count is refused, as the counts refuse it"
sha=$(counted "nix/suppressions-waiting.txt=src/old.rs 1 2")
expect refuses "${sha}" "is not a path and a count above 0" \
  "a suppression line of three fields is refused"
sha=$(counted $'src/old.rs=#[allow(dead_code)]\n#[allow(unused)]' \
  "nix/suppressions-waiting.txt=src/old.rs 2")
expect refuses "${sha}" "src/old.rs 2, main has 1" \
  "a count raised above main's own is refused"
sha=$(counted $'nix/suppressions-waiting.txt=src/old.rs 1\n  # It waits on its owner.')
expect merges "${sha}" "holds main" \
  "an indented comment in a list is skipped, as the counts skip it"
sha=$(counted "src/old.rs=fn old() {}" "nix/suppressions-waiting.txt=# Waiting.")
expect merges "${sha}" "holds main" \
  "a head that fixes a suppression and takes its line off merges"
sha=$(counted "${loose}" ".gitattributes=*.rs working-tree-encoding=UTF-16LE")
expect refuses "${sha}" "src/loose.rs: 1, listed 0" \
  "a file is read as its blob, whatever encoding the head asks"
# Under that encoding git takes the file for changed, so it's left by force.
git checkout -q -f "${first}"
# A long file its head lists in a list main lacks, which main's counts
# alone would pass.
git checkout -q -B counted "${first}"
mkdir -p src
awk 'BEGIN { for (n = 1; n <= 801; n++) print "// line " n }' > src/long.rs
echo src/long.rs > nix/long-files-waiting.txt
git add src/long.rs nix/long-files-waiting.txt
git commit -q -m long
sha=$(git rev-parse HEAD)
pass "${sha}"
expect refuses "${sha}" "nix/long-files-waiting.txt: src/long.rs 1, main has 0" \
  "a list main lacks, once main has the rule, is an empty one"
echo "src/long.rs 1" > nix/long-files-waiting.txt
git commit -q -am "long, counted"
sha=$(git rev-parse HEAD)
pass "${sha}"
expect refuses "${sha}" "nix/long-files-waiting.txt: src/long.rs 1 1, main has 0" \
  "a long-files line is one path, read whole as the counts read it"
echo "src/long.rs " > nix/long-files-waiting.txt
git commit -q -am "long, padded"
sha=$(git rev-parse HEAD)
pass "${sha}"
expect refuses "${sha}" "\"src/long.rs \" begins or ends with a space" \
  "a list line with a space at an end is refused, not keyed as its trim"
printf 'src/long.rs\t\n' > nix/long-files-waiting.txt
git commit -q -am "long, ending in a tab"
sha=$(git rev-parse HEAD)
pass "${sha}"
expect refuses "${sha}" "begins or ends with a space" \
  "a list line ending in a tab is refused, as one ending in a space is"
# A crowded folder named from a space outside ASCII and a hash, listed so:
# a comment to an awk reading UTF-8, but an entry to the counts, which
# would take the folder as listed and let it merge. A Mac's awk reads a
# no-break space as a space, and gawk an ideographic one. Without the UTF-8
# locale, or with an awk that reads no multibyte classes, as mawk and
# BusyBox's don't, the awk on the path would read neither so, and both
# cases would pass whether or not the gate reads under C, so that fails
# here first.
spaced=$(LC_ALL=en_US.UTF-8 awk '/^[[:space:]]+#/ { n++ } END { print n + 0 }' \
  <<< $'\302\240#\n\343\200\200#')
if [[ ${spaced} == 0 ]]; then
  echo "FAIL the crowd cases can't run: this awk reads neither space as one under" \
    "en_US.UTF-8 (no UTF-8 locale, or an awk without multibyte classes)" >&2
  failed=1
fi
for named in no-break=$'\302\240' ideographic=$'\343\200\200'; do
  git checkout -q -B counted "${first}"
  crowd="${named#*=}#crowd"
  mkdir "${crowd}"
  for n in $(seq 41); do : > "${crowd}/${n}.rs"; done
  printf '%s\n' "${crowd}" > nix/long-files-waiting.txt
  git add "${crowd}" nix/long-files-waiting.txt
  git commit -q -m "crowded, past a space outside ASCII"
  sha=$(git rev-parse HEAD)
  pass "${sha}"
  LC_ALL=en_US.UTF-8 expect refuses "${sha}" "#crowd 1, main has 0" \
    "a line opening with a space outside ASCII (${named%%=*}) is no comment, as the counts read it"
  git checkout -q -f "${first}"
  rm -rf "${crowd}"
done
# A list linked to a copy of main's outside the tree, which would merge
# were the link followed; read as the path it holds, it's no list line.
outside=$(mktemp -d)
git show "${first}:nix/suppressions-waiting.txt" > "${outside}/list"
git checkout -q -B counted "${first}"
ln -sf "${outside}/list" nix/suppressions-waiting.txt
git add nix/suppressions-waiting.txt
git commit -q -m linked
sha=$(git rev-parse HEAD)
pass "${sha}"
expect refuses "${sha}" "is not a path and a count above 0" \
  "a link is read as the path it holds, not followed"
rm -r "${outside}"
git checkout -q -f "${first}"

# A git that can't read a tree without its own attributes is refused.
mkdir "${work}/old-git"
real_git=$(command -v git)
cat > "${work}/old-git/git" << EOF
#!${BASH}
if [[ \$1 == version ]]; then echo "git version 2.41.0"; else exec "${real_git}" "\$@"; fi
EOF
chmod +x "${work}/old-git/git"
PATH="${work}/old-git:${PATH}" expect refuses "${head}" "run the gate with git 2.42 or later" \
  "a git before 2.42 is refused"

# A git that can't read one entry, main's rule, the head's or one of main's
# lists, stops the gate with its own status and words, rather than the gate
# reading it as a commit holding no such entry and refusing with its own.
# Each entry fails alone, so a read of any one put back in a condition
# shows.
mkdir "${work}/broken-git"
cat > "${work}/broken-git/git" << EOF
#!${BASH}
if [[ "\$*" == "ls-tree \${BROKEN_ENTRY}" ]]; then
  echo "fatal: the tree can't be read" >&2
  exit 128
fi
exec "${real_git}" "\$@"
EOF
chmod +x "${work}/broken-git/git"
pass "${head}"
for broken in "${first} -- nix/counts.sh" "${head} -- nix/counts.sh" \
  "${first} -- nix/long-files-waiting.txt" "${first} -- nix/suppressions-waiting.txt"; do
  code=0
  said=$(BROKEN_ENTRY=${broken} PATH="${work}/broken-git:${PATH}" \
    bash "${gate}" "${head}" 2>&1) || code=$?
  if ((code != 128)) || [[ ${said} != *"the tree can't be read"* ]]; then
    echo "FAIL a git error reading ${broken#* -- } at ${broken:0:12} stops the gate" \
      "with git's status and words: it ended ${code}, saying: ${said}" >&2
    failed=1
  fi
done

# Run outside any checkout, the gate stops with git's status and words.
# The ceiling keeps git from looking above ${work}, so a checkout holding
# the temporary folder can't stand in for the one the gate needs.
mkdir "${work}/outside"
cd "${work}/outside"
code=0
said=$(GIT_CEILING_DIRECTORIES=${work} bash "${gate}" "${head}" 2>&1) || code=$?
if ((code != 128)) || [[ ${said} != *"not a git repository"* ]]; then
  echo "FAIL the gate run outside any checkout stops with git's status and words:" \
    "it ended ${code}, saying: ${said}" >&2
  failed=1
fi
cd "${work}/tree"

# A main without the rule, as before it first came in or were it taken
# off: a head's own rule would judge the head by whatever it brought, so a
# head is refused, with the rule or without.
git init -q --bare -b main "${work}/bare.git"
git clone -q "${work}/bare.git" "${work}/bare" 2> /dev/null
cd "${work}/bare"
commit bare > /dev/null
git push -q origin HEAD:main
ruleless=$(commit ruleless)
pass "${ruleless}"
expect refuses "${ruleless}" "main has no nix/counts.sh" \
  "a head without the rule is refused where main has none"
# A head bringing a rule that passes anything, and a lint turned off with
# no list, which its own rule would let merge.
mkdir nix src
echo 'exit 0' > nix/counts.sh
echo '#[allow(dead_code)]' > src/loose.rs
git add nix src
git commit -q -m ruled
ruled=$(git rev-parse HEAD)
pass "${ruled}"
expect refuses "${ruled}" "main has no nix/counts.sh" \
  "a head bringing its own rule is refused where main has none"
cd "${work}/tree"

# Main keeping a list as a folder, which is no list: git would show the
# names it holds, and one named as a head's list line would let that line
# pass.
git init -q --bare -b main "${work}/folded.git"
git clone -q "${work}/folded.git" "${work}/folded" 2> /dev/null
cd "${work}/folded"
mkdir -p nix/suppressions-waiting.txt
cp "${counts}" nix/counts.sh
: > "nix/suppressions-waiting.txt/loose.rs 1"
git add nix
commit folded > /dev/null
git push -q origin HEAD:main
git rm -q -r nix/suppressions-waiting.txt
echo '#[allow(dead_code)]' > loose.rs
echo 'loose.rs 1' > nix/suppressions-waiting.txt
git add loose.rs nix
git commit -q -m listed
listed=$(git rev-parse HEAD)
pass "${listed}"
expect refuses "${listed}" "loose.rs 1, main has 0" \
  "a list main keeps as a folder is no list, not the names it holds"
cd "${work}/tree"

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
# Dated far ahead, as an author's clock may be, which the train mustn't
# take for its own, and with a subject a glob would read otherwise.
GIT_COMMITTER_DATE="2524608000 +0000" git -C "${work}/peer" commit -q -m 'peer [x] \*'
peer=$(git -C "${work}/peer" rev-parse HEAD)
git -C "${work}/peer" push -q origin HEAD:peer

before=$(date +%s)
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
dated=$(git log -1 --format=%ct "${tip}")
after=$(date +%s)
if ((dated < before || dated > after)); then
  echo "FAIL the train is dated when it's made, ${before} to ${after}, not as" \
    "a rider says: ${dated}" >&2
  failed=1
fi
earliest=$(git log -1 --format=%ct "${tip}^1^1")
if ((earliest != dated)); then
  echo "FAIL each of the train's merges is dated alike: ${earliest} and ${dated}" >&2
  failed=1
fi
held=$(git show "${tip}:north" "${tip}:south" "${tip}:peer")
if [[ ${held} != north$'\n'south$'\n'peer ]]; then
  echo "FAIL the train's tip holds every rider's change: ${held}" >&2
  failed=1
fi
points "refs/heads/train/${tip:0:12}" "${tip}" \
  "the train's tip is pushed for a builder to fetch"
rode goes "already made as train/${tip:0:12}" "the same train made again" \
  make "${north}" "${south}" "${peer}"
again=$(made)
if [[ ${again} != "${tip}" ]]; then
  echo "FAIL the same train made again on the same main is the same tip," \
    "so its checks still count: ${again} against ${tip}" >&2
  failed=1
fi
# Pushed by anyone, with the train's parents but another tree, and named
# to be found before it: it is never taken for the train.
forged=$(git commit-tree "${base}^{tree}" -p "${tip}^1" -p "${peer}" -m forged)
git push -q origin "${forged}:refs/heads/train/000000000000"
rode goes "already made as train/${tip:0:12}" \
  "a train is made again as the one holding its tree" \
  make "${north}" "${south}" "${peer}"
again=$(made)
if [[ ${again} != "${tip}" ]]; then
  echo "FAIL a pushed train whose tree differs is never the train made:" \
    "${again} against ${tip}" >&2
  failed=1
fi
git push -q origin --delete train/000000000000
# Pushed with the train's riders, words and tip's tree, its first two
# merges dated `$1` and its tip `$3`, or `$1` too, and holding the tree
# `$2` halfway.
forge() {
  local first second
  first=$(git log -1 --format=%B "${tip}^1^1" |
    GIT_AUTHOR_DATE="$1 +0000" GIT_COMMITTER_DATE="$1 +0000" \
      git commit-tree "${tip}^1^1^{tree}" -p "${base}" -p "${north}" -F -)
  second=$(git log -1 --format=%B "${tip}^1" |
    GIT_AUTHOR_DATE="$1 +0000" GIT_COMMITTER_DATE="$1 +0000" \
      git commit-tree "$2" -p "${first}" -p "${south}" -F -)
  git log -1 --format=%B "${tip}" |
    GIT_AUTHOR_DATE="${3:-$1} +0000" GIT_COMMITTER_DATE="${3:-$1} +0000" \
      git commit-tree "${tip}^{tree}" -p "${second}" -p "${peer}" -F -
}
# A tip on the train's own two merges, dated as it is, with `$@` for
# parents after them and words of its own where `words` says.
retip() {
  local words=${words:-$(git log -1 --format=%B "${tip}")}
  GIT_AUTHOR_DATE="${stamp} +0000" GIT_COMMITTER_DATE="${stamp} +0000" \
    git commit-tree "${tip}^{tree}" -p "${tip}^1" "$@" -m "${words}"
}
# Whether making the train again takes `forged`, pushed as `why` says,
# for the train made.
refused() {
  local forged=$1 why=$2
  git push -q origin "${forged}:refs/heads/train/000000000000"
  rode goes "already made as train/${tip:0:12}" "${why}" \
    make "${north}" "${south}" "${peer}"
  again=$(made)
  if [[ ${again} != "${tip}" ]]; then
    echo "FAIL ${why}, is never the train made: ${again} against ${tip}" >&2
    failed=1
  fi
  git push -q origin --delete train/000000000000
}
stamp=$(git log -1 --format=%ct "${tip}")
main_date=$(git log -1 --format=%ct "${base}")
record="$(git rev-parse --path-format=absolute --git-common-dir)/merge-trains"
# Merged as the train was and dated as main is, which only a forger can
# make before the last train lands: this checkout never pushed it.
forged=$(forge "${main_date}" "${tip}^1^{tree}")
refused "${forged}" "a train pushed by another, merged as this one was"
# Each of these is taken for one this checkout pushed, so what is
# checked of every merge is what refuses it.
for case in \
  "another tree halfway:$(forge "${stamp}" "${base}^{tree}")" \
  "dated before main:$(forge $((main_date - 1)) "${tip}^1^{tree}")" \
  "dated past now:$(forge 2524608000 "${tip}^1^{tree}")" \
  "its merges dated apart:$(forge $((stamp - 1)) "${tip}^1^{tree}" "${stamp}")" \
  "another rider:$(retip -p "${south}")" \
  "one parent:$(retip)" \
  "other words:$(words=other retip -p "${peer}")"; do
  echo "${case##*:}" >> "${record}"
  refused "${case##*:}" "a train this checkout pushed with ${case%%:*}"
done
# Other riders on the same main make another train.
rode goes "riding ${north:0:12} ${south:0:12};" "a train of other riders" \
  make "${north}" "${south}"
other=$(made)
if [[ ${other} == "${tip}" || ${said} == *"already made"* ]]; then
  echo "FAIL a train of other riders is made anew, not taken: ${said}" >&2
  failed=1
fi
git push -q origin --delete "train/${other:0:12}"

rode refuses "nothing kept in ${MERGE_GATE_PASSED} says" \
  "a train nothing passed at doesn't land" land "${tip}"
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
