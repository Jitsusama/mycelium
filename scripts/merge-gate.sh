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
  agents-cargo-goes-through-nix-offload
  counts-name-their-lines
  counts-only-fall
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
# Named from where the gate was run, before it moves to the checkout's top.
if [[ ${passed} != /* ]]; then
  passed=${PWD}/${passed}
fi

# From the checkout's top, since git reads a path given to it, and checks
# an index out, from the folder it runs in. Read outside the `cd`, so a
# folder outside any checkout stops the gate in git's words.
top=$(git rev-parse --show-toplevel)
cd "${top}"
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

# The counts that only fall (nix/counts.sh), judged by main's copy of the
# rule over the head's own tree, each of the head's lists only shrinking
# against main's but by the lines a sharper reader it brings sees on main,
# as said below, so a head can neither loosen the rule it's judged by nor
# list its own new entry; a looser rule it brings judges only the heads
# after it, as a change to the rule, read by its review. The train lands
# only as this lets its tip, so a train's merged tree is judged too. Main
# must have the rule, since a head's own copy would judge the head by
# whatever it brought, and a list main lacks is an empty one. The head
# must keep the rule as a file too, or once it merged main would have none
# and every head after it would be refused. The tree is read from an index
# of the gate's own, its attributes from the empty tree so none of the
# head's changes a file from its blob, and each link written as a file
# holding its target, so none reaches outside the copy. A git before 2.42
# ignores GIT_ATTR_SOURCE and would apply the head's attributes, so it's
# refused.
version=$(git version | sed -n 's/^git version \([0-9]*\.[0-9]*\).*/\1/p')
if ! printf '%s\n' 2.42 "${version}" | sort -C -t . -k 1,1n -k 2,2n; then
  echo "merge-gate: git ${version:-of no version} can't read a tree without its" \
    "own attributes; run the gate with git 2.42 or later." >&2
  exit 1
fi
scratch=$(mktemp -d)
trap 'rm -rf "${scratch}"' EXIT
empty_tree=4b825dc642cb6eb9a060e54bf8d69288fbee4904
GIT_INDEX_FILE="${scratch}/index" git read-tree "${sha}"
GIT_INDEX_FILE="${scratch}/index" GIT_ATTR_SOURCE="${empty_tree}" \
  git -c core.symlinks=false checkout-index -a --prefix="${scratch}/tree/"
rule=nix/counts.sh
# Each commit's entry for the rule, read outside any condition so a commit
# git can't read stops the gate with git's own words. Only a file's mode
# holds it as a file, rather than as a link or not at all.
main_rule=$(git ls-tree "${main}" -- "${rule}")
head_rule=$(git ls-tree "${sha}" -- "${rule}")
as_file='^100(644|755) blob '
if [[ ! ${main_rule} =~ ${as_file} ]]; then
  echo "merge-gate: main has no ${rule}, so nothing of main's can judge" \
    "${sha:0:12}'s counts: put the rule back on main first." >&2
  exit 1
fi
if [[ ! ${head_rule} =~ ${as_file} ]]; then
  echo "merge-gate: ${sha:0:12} doesn't keep ${rule} as a file, so main would" \
    "have nothing to judge the next head by: keep the rule." >&2
  exit 1
fi
git show "${main}:${rule}" > "${scratch}/counts.sh"
raised=""
for list in nix/long-files-waiting.txt nix/suppressions-waiting.txt; do
  # Main's entry read outside any condition, as the rule's is; one that
  # isn't a file is no list, the strictest reading.
  main_entry=$(git ls-tree "${main}" -- "${list}")
  main_list=""
  if [[ ${main_entry} =~ ${as_file} ]]; then
    main_list=$(git show "${main}:${list}")
  fi
  head_list=""
  if [[ -f "${scratch}/tree/${list}" ]]; then
    head_list=$(< "${scratch}/tree/${list}")
  fi
  # Each line against main's, read as counts.sh reads it: the long list's
  # whole, as one path, and the suppressions' as "path count". One main
  # lacks, or a count above main's, raises the list, and a suppression line
  # that isn't a path and a count above 0 is refused as it stands, as is a
  # line with a space at either end, which counts.sh would read as another
  # path than the one it's keyed by here.
  whole=0
  if [[ ${list} == nix/long-files-waiting.txt ]]; then whole=1; fi
  # Under LC_ALL=C, as counts.sh reads the lists, so a space outside ASCII
  # is no space to either.
  raised+=$(LC_ALL=C awk -v list="${list}" -v whole="${whole}" '
    { line = $0; sub(/^[[:space:]]+/, ""); sub(/[[:space:]]+$/, "") }
    /^#/ || $0 == "" { next }
    whole { key = $0; n = 1 }
    !whole { key = $1; n = NF > 1 ? $2 : 1 }
    FNR == NR { main[key] = n; next }
    $0 != line { print list ": \"" line "\" begins or ends with a space"; next }
    !whole && (NF != 2 || $2 !~ /^[1-9][0-9]*$/) {
      print list ": \"" $0 "\" is not a path and a count above 0"
      next
    }
    n + 0 > main[key] + 0 && whole { print list ": " key " " n ", main has " main[key] + 0 }
    n + 0 > main[key] + 0 && !whole { print "rise " key " " main[key] + 0 " " n }
  ' <(printf '%s\n' "${main_list}") <(printf '%s\n' "${head_list}"))$'\n'
done
rises=$(sed -n 's/^rise //p' <<< "${raised}")
raised=$(sed '/^$/d; /^rise /d' <<< "${raised}")
if [[ -n ${raised} ]]; then
  echo "${raised}" >&2
  echo "merge-gate: ${sha:0:12} raises a list that only shrinks, above main's:" \
    "fix the code the line names rather than listing it." >&2
  exit 1
fi

# A suppression line may rise only where a head brings a reader that sees
# more than main's in a Rust file the head leaves as main has it, and only
# by lines that reader counts there and main's doesn't. What that reader
# counts is bounded by its review, as any change to the rule is: one
# counting a line that turns no lint off is a looser rule, which its
# review catches, so the gate doesn't read the lines itself. That review
# reads the head's nix/counts.sh and the checks counts-name-their-lines
# and counts-only-fall in its nix/checks.nix, and whatever its flake.nix
# and flake.lock make of them, since those decide what it names. The
# head's reader runs only as counts-name-their-lines, whose output names
# each line it counts; main's reader runs here, as it does below, with
# each risen line put back as main lists it, so no head lists a
# suppression it adds.
suppression_list=nix/suppressions-waiting.txt
# Every check must be kept as passed at the head before any rise is
# judged, since judging one builds from the head's flake.
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
  echo "merge-gate: nothing kept in ${passed} says these passed at ${sha:0:12}:" \
    "${missing[*]}. Build them there with nix-offload there . ${missing[*]}" >&2
  exit 1
fi
# What each rise was let through by, said once every other check passes.
let_rise=""
if [[ -n ${rises} ]]; then
  said_rises=$(awk -v list="${suppression_list}" '{ print list ": " $1 " " $3 ", main has " $2 }' \
    <<< "${rises}")
  if [[ ${main_rule} == "${head_rule}" ]]; then
    echo "${said_rises}" >&2
    echo "merge-gate: ${sha:0:12} raises a list that only shrinks, above main's," \
      "with main's own reader: fix the code the line names rather than listing it." >&2
    exit 1
  fi
  while read -r file _ _; do
    # Git's own name for the key, read with no pathspec magic and unquoted,
    # must be the key: a path through ./ or ../, a doubled slash or a magic
    # word names the file to git but another, or none, to its readers.
    listed=$(git --literal-pathspecs ls-tree -z --full-tree --name-only "${sha}" -- "${file}" \
      | tr -d '\0') || listed=""
    if [[ ${listed} != "${file}" ]]; then
      echo "${said_rises}" >&2
      echo "merge-gate: ${sha:0:12} raises ${file}'s count, naming it other than as git" \
        "does, so it isn't the file its readers count: name it by its path from the top." >&2
      exit 1
    fi
    # Each entry as mode and object, read as the key is, so a link or
    # folder named as Rust is no Rust file, and a head that changes either
    # doesn't leave it alone.
    main_blob=$(git --literal-pathspecs ls-tree --format='%(objectmode) %(objectname)' \
      "${main}" -- "${file}")
    head_blob=$(git --literal-pathspecs ls-tree --format='%(objectmode) %(objectname)' \
      "${sha}" -- "${file}")
    if [[ ${file} != *.rs
      || -n ${main_blob} && ${main_blob} != 100644\ * && ${main_blob} != 100755\ * ]]; then
      echo "${said_rises}" >&2
      echo "merge-gate: ${sha:0:12} raises ${file}'s count, and only a Rust file's" \
        "may rise: fix the code rather than listing it." >&2
      exit 1
    fi
    if [[ -z ${main_blob} || ${main_blob} != "${head_blob}" ]]; then
      echo "${said_rises}" >&2
      echo "merge-gate: ${sha:0:12} raises ${file}'s count but doesn't leave the file as" \
        "main has it, so the rise may hold a lint it turns off itself: fix the code" \
        "rather than listing it." >&2
      exit 1
    fi
  done <<< "${rises}"
  # MERGE_GATE_BUILD_SECONDS bounds the head's build below, 1800 seconds
  # unless it says otherwise.
  limit=${MERGE_GATE_BUILD_SECONDS:-1800}
  if [[ ! ${limit} =~ ^[1-9][0-9]*$ ]]; then
    echo "merge-gate: MERGE_GATE_BUILD_SECONDS is ${limit}, not a count of seconds" \
      "above 0, so the head's build can't be bounded." >&2
    exit 1
  fi
  if ! command -v timeout > /dev/null; then
    echo "merge-gate: no timeout command is on this machine's path to bound the build" \
      "of ${sha:0:12}'s reader, so its rise can't be judged: put coreutils' on it." >&2
    exit 1
  fi
  if ! command -v nix > /dev/null; then
    echo "merge-gate: no nix command is on this machine's path to build ${sha:0:12}'s" \
      "reader, so its rise can't be judged: put nix on it." >&2
    exit 1
  fi
  # Main's reader judges the head first, below, with each risen file put
  # back at main's count, so a lint the head turns off itself is refused
  # before anything is built from the head.
  awk 'FNR == NR { if (NF) risen[$1] = 1; next } !($1 in risen)' \
    <(printf '%s\n' "${rises}") "${scratch}/tree/${suppression_list}" > "${scratch}/list"
  awk '$2 > 0 { print $1, $2 }' <<< "${rises}" >> "${scratch}/list"
  mv "${scratch}/list" "${scratch}/tree/${suppression_list}"
fi
if ! counted=$(cd "${scratch}/tree" \
  && GIT_DIR="${scratch}/none" bash "${scratch}/counts.sh" 2>&1 < /dev/null); then
  echo "${counted}" >&2
  echo "merge-gate: main's nix/counts.sh refuses ${sha:0:12}; above says why." >&2
  exit 1
fi
if [[ -n ${rises} ]]; then
  # The head's flake builds its reader only once the keeper kept every
  # check as passed there, above, each built from that flake, so the
  # flake is trusted here as far as those builds of it were. This machine
  # still evaluates it, and builds what it writes, under its own nix
  # settings, so the check runs sandboxed only as they sandbox a build;
  # the flake's own settings are refused. The build says why it failed on
  # its own, as the head's reader, its flake or this machine may be at
  # fault. It's bounded, so a head whose flake never finishes stops the
  # gate rather than the train: nix is killed a minute past the limit if
  # it hasn't stopped. It stays in the gate's own process group, so an
  # interrupt that stops the gate stops it too.
  # The build's path goes to a file, not a pipe, so the gate goes on once
  # timeout returns, whatever nix left running that holds the build's
  # output. Nix's errors go where the gate's go, so as to be seen as they
  # come, and a stray holding those holds only a caller reading them to
  # their end. The train doesn't, but it passes them on, so one reading
  # the train's errors to their end is held.
  status=0
  timeout --foreground --kill-after=60 "${limit}" nix build --no-link --print-out-paths \
    --option accept-flake-config false \
    "git+file://${top}?rev=${sha}#checks.${system}.counts-name-their-lines" \
    < /dev/null > "${scratch}/built" || status=$?
  built=$(< "${scratch}/built")
  if ((status == 124)); then
    echo "merge-gate: ${sha:0:12}'s reader was still building after ${limit} seconds," \
      "so its rise can't be judged: see why its flake takes so long, or raise" \
      "MERGE_GATE_BUILD_SECONDS." >&2
    exit 1
  fi
  # Timeout passes on the signal that killed nix, so a kill from this
  # machine, as for want of memory, comes back as timeout's own does.
  if ((status == 137)); then
    echo "merge-gate: ${sha:0:12}'s reader was killed, by timeout a minute past" \
      "${limit} seconds or by this machine, so its rise can't be judged: see whether" \
      "this machine ran short of memory, or why its flake takes so long." >&2
    exit 1
  fi
  if ((status >= 125 && status <= 127)); then
    echo "merge-gate: this machine couldn't start the build of ${sha:0:12}'s reader" \
      "(status ${status}), so its rise can't be judged: timeout or nix says why above." >&2
    exit 1
  fi
  if ((status != 0)); then
    echo "merge-gate: ${sha:0:12}'s reader couldn't be built here as" \
      "checks.${system}.counts-name-their-lines, so its rise can't be judged. Nix" \
      "says why above: fix the head if its reader or flake fails, or land it from" \
      "a machine that builds ${system}." >&2
    exit 1
  fi
  # Only a file the build wrote, never a link out of the store.
  if [[ -L ${built} || ! -f ${built} ]]; then
    echo "merge-gate: ${sha:0:12}'s counts-name-their-lines wrote no file, so its" \
      "rise can't be read." >&2
    exit 1
  fi
  head_named=$(< "${built}")
  # Read outside any condition, so a reader that fails stops the gate in
  # its own words.
  main_named=$(cd "${scratch}/tree" \
    && GIT_DIR="${scratch}/none" bash "${scratch}/counts.sh" --name-lines < /dev/null)
  unread=$(grep -Evx '[^[:space:]]+ [1-9][0-9]*' <<< "${head_named}"$'\n'"${main_named}" \
    | sed '/^$/d' || true)
  if [[ -n ${unread} ]]; then
    echo "${unread}" >&2
    echo "merge-gate: the readers named these lines as other than PATH LINE, so" \
      "${sha:0:12}'s rise can't be read." >&2
    exit 1
  fi
  # Each risen file, its counts on main's list and the head's, whether the
  # lines only the head's reader counts there are enough for the rise, how
  # many they are, and each once. For a reader whose names agree with its
  # count they always are, since its kept counts-only-fall holds its count
  # to the head's list and main's reader counted the file at main's. So
  # this only checks that the lines it names beyond main's cover the rise;
  # its review bounds the rise itself. Awk weighs the rise, as bash's sums
  # wrap past 64 bits and so would let a count that long rise unnamed.
  judged=$(awk '
    FNR == 1 { part++ }
    NF == 0 { next }
    part == 1 { rise[$1] = $2 " " $3; from[$1] = $2; to[$1] = $3; next }
    part == 2 { main[$1 " " $2] = 1; next }
    ($1 in rise) && !(($1 " " $2) in main) && !seen[$1 " " $2]++ {
      only[$1] = only[$1] " " $2
      n[$1]++
    }
    END {
      for (file in rise) {
        fits = n[file] + 0 >= to[file] - from[file] ? "fits" : "short"
        print file, rise[file], fits, n[file] + 0 only[file]
      }
    }
  ' <(printf '%s\n' "${rises}") <(printf '%s\n' "${main_named}") \
    <(printf '%s\n' "${head_named}"))
  while read -r file from to fits count lines; do
    if [[ ${fits} != fits ]]; then
      echo "${said_rises}" >&2
      echo "merge-gate: ${sha:0:12} raises ${file} from ${from} to ${to}, but only" \
        "${count} of the lines its reader counts there are lines main's doesn't." >&2
      exit 1
    fi
    let_rise+="merge-gate: ${suppression_list} lets ${file} rise from ${from} to ${to}:"
    let_rise+=" ${sha:0:12}'s reader counts lines ${lines} there, and main's doesn't."$'\n'
  done <<< "${judged}"
fi

printf '%s' "${let_rise}"

if grep -qF '"check":"checks.aarch64-darwin.mycelium"' <<< "${kept}"; then
  echo "merge-gate: ${sha:0:12} holds main, every Linux check passed there," \
    "and a Mac built it."
else
  echo "merge-gate: ${sha:0:12} holds main and every Linux check passed" \
    "there. No Mac built it: say so in the Validation, and build" \
    "checks.aarch64-darwin.mycelium before core.nix takes it."
fi
