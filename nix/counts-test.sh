#!/usr/bin/env bash
# The cases of nix/counts.sh itself, each over a tree made for it, most
# with no git, as scripts/merge-gate.sh runs it, and a few in a repository,
# as the flake's check runs it: what it counts long, crowded and
# turning a linter off, each as core.nix's code-health (its measure.rs)
# counts it. A second argument names the bash to run it under, as a Mac's
# /bin/bash 3.2, which a gate there may run it with.
set -euo pipefail

usage="counts-test.sh COUNTS [BASH]"
counts=$(realpath "${1:?Name the counts: ${usage}}")
shell=${2:-bash}
failed=0
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT
made=0
dir=""
git_dir=""

# A fresh tree for the next case, with no git unless it's made one.
tree() {
  made=$((made + 1))
  dir="${work}/${made}"
  git_dir="${dir}/none"
  mkdir -p "${dir}/nix"
}
# The tree made a repository, whose files are what git keeps or would.
repository() {
  git -C "${dir}" init -q
  git_dir="${dir}/.git"
}
# A file at `path` holding one line of `text`.
put() {
  mkdir -p "$(dirname "${dir}/$1")"
  printf '%s\n' "$2" > "${dir}/$1"
}
# A file at `path` of `count` lines, line `at` holding `text`.
lines() {
  mkdir -p "$(dirname "${dir}/$1")"
  awk -v count="$2" -v at="${3:-0}" -v text="${4:-}" \
    'BEGIN { for (n = 1; n <= count; n++) print (n == at ? text : "# line " n) }' \
    > "${dir}/$1"
}
# Whether the counts pass the tree or fail it, saying `words`.
expect() {
  local meant=$1 words=$2 why=$3 said code=0
  said=$(cd "${dir}" && GIT_DIR="${git_dir}" "${shell}" "${counts}" 2>&1) || code=$?
  local did=passes
  ((code == 0)) || did=fails
  if [[ ${did} != "${meant}" || ${said} != *"${words}"* ]]; then
    echo "FAIL ${why}: it ${did}, saying: ${said}" >&2
    failed=1
  fi
}

tree
lines a.sh 800
expect passes "" "800 lines is not past the line"
tree
lines a.sh 801
expect fails "a.sh" "801 lines is"
tree
lines a=1.sh 801
expect fails "a=1.sh" "a file named as an assignment is read, not assigned"
tree
lines a.sh 801 40 "# Long because it reads one file whole."
expect passes "" "a reason in the first 40 lines says why"
tree
lines a.sh 801 41 "# Long because it reads one file whole."
expect fails "a.sh" "a reason past the first 40 lines doesn't"
tree
lines a.sh 801 1 "# It is Long because it reads one file."
expect fails "a.sh" "a reason must open a sentence"
tree
lines a.sh 801 1 "# It reads one file. Long because of that."
expect passes "" "a reason may open a later sentence"
tree
lines a.sh 801 1 $'# Long because\303\251'
expect fails "a.sh" "a letter past ASCII straight after isn't the words"
tree
lines a.sh 801 1 $'# Long because\302\240it reads one file.'
expect passes "" "a no-break space after the words ends them"
tree
lines a.sh 801 1 $'# Long because\342\200\246 it reads one file.'
expect passes "" "punctuation past ASCII after the words ends them, read as bytes"
tree
lines a.sh 801 1 $'# Long because\303\227 it reads one file.'
expect passes "" "a times sign after the words ends them, though its lead byte is a letter's"
tree
lines a.sh 801 1 $'# Long because\302\241 it reads one file.'
expect passes "" "Latin-1 punctuation after the words ends them"
tree
lines a.sh 801 1 $'# Long because\302\252'
expect fails "a.sh" "a Latin-1 letter after the words isn't the words"

tree
put b=2.sh "# shellcheck disable=SC2086"
expect fails "b=2.sh: 1, listed 0" "a suppression in a file named as an assignment counts"
tree
put x.ts $'//\302\240biome-ignore lint: it reads a stream'
expect fails "x.ts: 1, listed 0" "a no-break space after the mark is whitespace"
tree
put x.ts 'const said = "// biome-ignore";'
expect passes "" "a mark inside a string is no comment"
tree
put x.rs '    #[allow(dead_code)]'
expect fails "x.rs: 1, listed 0" "an indented attribute counts"
tree
put x.rs '#[expect(dead_code, reason = "it waits on its caller")]'
expect fails "x.rs: 1, listed 0" "an expect with its reason counts, as an allow does"
tree
put x.rs '// #[allow(dead_code)]'
expect passes "" "an attribute in a comment doesn't"
tree
put "x y.ts" "export const a = 1;"
expect fails "paths hold a space" "a code file whose path a list can't name is refused"
tree
repository
put "x y.ts" "export const a = 1;"
expect fails "paths hold a space" "so is one git lists"
tree
repository
lines skipped.sh 801
put .gitignore "skipped.sh"
expect passes "" "in a repository, a file git ignores isn't counted"

tree
put x.ts "// biome-ignore lint: it reads a stream"
put nix/suppressions-waiting.txt "x.ts 1"
expect passes "" "a suppression listed at its count passes"
put nix/suppressions-waiting.txt "x.ts 2"
expect fails "x.ts 1 (listed 2)" "a count listed above what's found must fall"
put nix/suppressions-waiting.txt "x.ts many"
expect fails "aren't a file and a count above 0" "a count that isn't one is refused"

tree
lines a.sh 801
put nix/long-files-waiting.txt "a.sh"
expect passes "" "a long file listed passes"
put nix/long-files-waiting.txt "b.sh"
expect fails "no longer past the line" "a listed file that isn't long must come off"

tree
for ((n = 1; n <= 40; n++)); do
  put "crowd/${n}.md" "${n}"
done
expect passes "" "40 entries is not past the line"
put crowd/README.md $'# Crowd\n\nMany\nentries because each is a case.'
expect passes "" "a folder's note says why, its words joined across lines"
put crowd/README.md $'\303\227 \303\251Many entries because each is a case.'
expect fails "crowd" "a letter past ASCII just before the words isn't the words"
put crowd/README.md $'a \303\227Many entries because each is a case.'
expect passes "" "a times sign just before the words starts them"

# A folder whose name ends in a space, listed with it, which the gate would
# key as the trimmed name main may already list.
tree
for ((n = 1; n <= 41; n++)); do
  put "crowd /${n}.md" "${n}"
done
put nix/long-files-waiting.txt "crowd "
expect fails "begin or end with a space" "a list line with a space at an end is refused"

# A crowded folder named from a space outside ASCII and a hash, listed so:
# an entry to the counts, which read bytes whatever the locale, though a
# grep reading UTF-8 would take the line for a comment and the folder for
# one unlisted. A case runs only where grep reads its space as one under
# en_US.UTF-8, since elsewhere it would pass whether or not the counts read
# bytes, and a C library may class the no-break space as none. Where grep
# reads neither, for want of a UTF-8 locale or of multibyte classes, that
# fails here.
ran=0
for named in no-break=$'\302\240' ideographic=$'\343\200\200'; do
  spaced=$(LC_ALL=en_US.UTF-8 grep -c '^[[:space:]]' <<< "${named#*=}#" || true)
  if [[ ${spaced} != 1 ]]; then
    echo "counts-test: the ${named%%=*} case is skipped, since grep reads no" \
      "${named%%=*} space as one under en_US.UTF-8"
    continue
  fi
  ran=$((ran + 1))
  tree
  crowd="${named#*=}#crowd"
  for ((n = 1; n <= 41; n++)); do
    put "${crowd}/${n}.md" "${n}"
  done
  put nix/long-files-waiting.txt "${crowd}"
  LC_ALL=en_US.UTF-8 expect passes "" \
    "a line opening with a space outside ASCII (${named%%=*}) is an entry, whatever the locale"
done
if ((ran == 0)); then
  echo "FAIL the locale cases can't run: grep reads neither space as one under" \
    "en_US.UTF-8 (no UTF-8 locale, or a grep without multibyte classes)" >&2
  failed=1
fi

if ((failed == 0)); then
  echo "counts-test: every case holds"
fi
exit "${failed}"
