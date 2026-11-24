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
# against main's, so a head can neither loosen the rule it's judged by nor
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
    n + 0 > main[key] + 0 { print list ": " key " " n ", main has " main[key] + 0 }
  ' <(printf '%s\n' "${main_list}") <(printf '%s\n' "${head_list}"))$'\n'
done
raised=$(sed '/^$/d' <<< "${raised}")
if [[ -n ${raised} ]]; then
  echo "${raised}" >&2
  echo "merge-gate: ${sha:0:12} raises a list that only shrinks, above main's:" \
    "fix the code the line names rather than listing it." >&2
  exit 1
fi
if ! counted=$(cd "${scratch}/tree" \
  && GIT_DIR="${scratch}/none" bash "${scratch}/counts.sh" 2>&1 < /dev/null); then
  echo "${counted}" >&2
  echo "merge-gate: main's nix/counts.sh refuses ${sha:0:12}; above says why." >&2
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
  echo "merge-gate: nothing kept in ${passed} says these passed at ${sha:0:12}:" \
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
