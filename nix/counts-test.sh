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
# Whether --name-lines says exactly `meant`, each line a suppression as
# `PATH LINE`, and compares nothing.
names() {
  local meant=$1 why=$2 said code=0
  said=$(cd "${dir}" && GIT_DIR="${git_dir}" "${shell}" "${counts}" --name-lines 2>&1) || code=$?
  if ((code != 0)) || [[ ${said} != "${meant}" ]]; then
    echo "FAIL ${why}: it exits ${code}, saying: ${said}" >&2
    failed=1
  fi
}
# Whether --name-lines refuses the tree, saying exactly `meant`.
refuses_naming() {
  local meant=$1 why=$2 said code=0
  said=$(cd "${dir}" && GIT_DIR="${git_dir}" "${shell}" "${counts}" --name-lines 2>&1) || code=$?
  if ((code != 1)) || [[ ${said} != "${meant}" ]]; then
    echo "FAIL ${why}: it exits ${code}, saying: ${said}" >&2
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
# code-health's own case (attributes.rs), line for line, whose lines 1,
# 2, 3, 5, 9, 10, 17, 19, 23, 24, 26, 27, 29, 30 and 31 turn a lint off.
tree
put f.rs "$(printf '%s\n' \
  '#[cfg_attr(test, allow(dead_code))]' \
  '# [allow(unused)]' \
  '#![cfg_attr(not(test), expect(clippy::x))]' \
  '#[cfg_attr(test, derive(Debug))]' \
  '# ! [ expect ( x ) ]' \
  '#[cfg_attr(feature = "x", deny(y))]' \
  'let s = "#[cfg_attr(test, allow(x))]";' \
  '#[cfg_attr(test, disallow(x))]' \
  '#[cfg_attr(test, derive(Debug), allow(x))]' \
  '#[cfg_attr(a, cfg_attr(b, expect(x)))]' \
  '#[allow_internal_unstable(x)]' \
  '#[derive(Debug, allow(x))]' \
  '#[cfg_attr(test, derive(Debug))] // a, allow(x) later' \
  '#[cfg_attr(doc, doc = "a, allow(x) here")]' \
  '#[cfg_attr(feature = "a\", allow(b)", deny(x))]' \
  $'#\302\240[allow(x)]' \
  $'#\342\200\216[allow(x)]' \
  '#[cfg_attr(test, derive(Debug, allow(x)))]' \
  '#[cfg_attr(' \
  $'    not(test), // a comment\'s ) and ]' \
  '    expect(unused)' \
  ')]' \
  $'\342\200\216\357\273\277\t#[allow(x)]' \
  '#[derive(Debug)] #[allow(x)]' \
  '#[derive(Debug)]' \
  '#[allow(x)]' \
  '#[cfg_attr(feature = r#"a")"#, allow(x))]' \
  '#[cfg_attr(feature = r#"a", allow(x)"#, deny(x))]' \
  '# /* a, allow(y) */ [allow(x)]' \
  "#[cfg_attr(test, doc = ')', allow(x))]" \
  "#[cfg_attr(test, doc = '\\\"', allow(x))]" \
  '#[allow(x)')"
expect fails "f.rs: 15, listed 0" \
  "a lint turned off as Rust writes an attribute counts, and no other attribute does"
names "$(printf 'f.rs %s\n' 1 2 3 5 9 10 17 19 23 24 26 27 29 30 31)" \
  "each Rust line turning a lint off is named where its attribute opens"
tree
put g.rs '#[my_attr[x]] #[allow(y)]'
expect fails "g.rs: 1, listed 0" "an attribute holding brackets closes at its own"
tree
put h.rs "$(printf '%s\n' \
  '#[cfg_attr(test, doc[a, allow(x)])]' \
  '#[cfg_attr(test, doc{a, allow(x)})]' \
  '#[cfg_attr(test, doc[a], allow(y))]' \
  '#[cfg_attr(test, doc{a}, allow(y))]' \
  '#[cfg_attr(test, a}, allow(y))]')"
expect fails "h.rs: 3, listed 0" \
  "a cfg_attr is split only at commas outside its attributes' brackets"
names "$(printf 'h.rs %s\n' 3 4 5)" \
  "and is read on past their close, and past a close it never opened"
# Each even line from 2 to 34, and line 35, turns a lint off; those in
# code, right after what would hide them from a walk misreading it, count.
tree
put k.rs "$(printf '%s\n' \
  '/*' '#[allow(x)]' '*/' '#[allow(x)]' \
  'const S: &str = "' '#[allow(x)]' '";' '#[allow(x)]' \
  'const R: &str = r#"' '#[allow(x)]' '"#;' '#[allow(x)]' \
  '/* /* */' '#[allow(x)]' '*/' '#[allow(x)]' \
  "const Q: char = '\"'; const B: &[u8] = br\"\\\";" '#[allow(x)]' \
  "m!('outer\"\\\"" '#[allow(x)]' '");' '#[allow(x)]' \
  "m!('r\"\\\"" '#[allow(x)]' '");' '#[allow(x)]' \
  "m!('br\"\\\"" '#[allow(x)]' '");' '#[allow(x)]' \
  'm!(xbr"\"' '#[allow(x)]' '");' '#[allow(x)]' \
  '#[cfg_attr(test, doc = br"\", allow(x))]')"
expect fails "k.rs: 10, listed 0" \
  "a line opening inside a comment, string or raw string begun above doesn't count"
names "$(printf 'k.rs %s\n' 4 8 12 16 18 22 26 30 34 35)" \
  "and each line opening in code past one does"
# Lines 2 and 6 sit in a string after a raw name or lifetime, line 9's
# attribute holds one, line 11 follows a raw string, no raw name, lines
# 13, 16, 19 and 22 one past a number's or a character's suffix r, and
# line 24's attribute holds a number's suffix r, opening no raw string.
tree
put n.rs "$(printf '%s\n' \
  'm!(r#r"\";' '#[allow(x)]' '");' '#[allow(x)]' \
  "m!('r#r\"\\\";" '#[allow(x)]' '");' '#[allow(x)]' \
  '#[cfg_attr(test, m(r#r"\"]"), allow(x))]' 'const T: &str = r#"a "q"#;' '#[allow(x)]' \
  'm!(1r#r"\");' '#[allow(x)]' '// ");' \
  "m!('a'r#r\"\\\");" '#[allow(x)]' '// ");' \
  "m!('\\n'r#r\"\\\");" '#[allow(x)]' '// ");' \
  'm!(1ur#r"\");' '#[allow(x)]' '// ");' \
  '#[cfg_attr(all(), cfg_attr(any(), m(1r"\"),")), allow(x))]')"
expect fails "n.rs: 9, listed 0" \
  "a raw name's last letter opens no raw string"
names "$(printf 'n.rs %s\n' 4 8 9 11 13 16 19 22 24)" \
  "so a string after one is read as a string, and a suffix's r and # are no raw name"
# A lint level named rawly is rustc's own level.
tree
put p.rs "$(printf '%s\n' '#[r#allow(x)]' '#[cfg_attr(test, r#expect(x))]' '#![r#allow(x)]')"
names "$(printf 'p.rs %s\n' 1 2 3)" "a lint level named rawly turns the lint off"
# Line 2 follows a C raw string, line 5 a character past ASCII, and line
# 10 a raw name after Rust's whitespace past ASCII, whose string hides 8.
tree
put x.rs "$(printf '%s\n' \
  'm!(cr"\");' '#[allow(x)]' '// ");' \
  $'m!(\047\303\251\047 \047"\047);' '#[allow(x)]' '// "' \
  $'m!(\342\200\250r#r"\\");' '#[allow(x)]' '");' '#[allow(x)]')"
names "$(printf 'x.rs %s\n' 2 5 10)" \
  "a C raw string, a wide character and a raw name past wide whitespace read as rustc reads them"
# Each character here is read as rustc reads it, so line 2 is code in c,
# f, g, h and k.rs, and line 2 in d, e and i.rs, and line 3 in j.rs, sit
# in a string. c.rs's byte character opens no lifetime, though b.rs's
# suffix ends where its quote stands, since each file is read alone,
# unlike d.rs's after a raw name ending b, e.rs's after a suffix ending b
# and i.rs's after a name ending b; a quote closes f.rs's name, but not
# k.rs's, which Rust's whitespace past ASCII ends; g.rs's escape passes a
# quote, h.rs's a wide character; and j.rs's character reads on past a
# line's end a quote follows.
tree
put b.rs $'\047a\047b'
put c.rs "$(printf '%s\n' $'m!(b\047a"\047);' '#[allow(x)]' '// "')"
put d.rs "$(printf '%s\n' $'m!(r#b\047a"\047);' '#[allow(x)]' '");' '#[allow(x)]')"
put e.rs "$(printf '%s\n' $'m!("x"b\047a"\047);' '#[allow(x)]' '");' '#[allow(x)]')"
put f.rs "$(printf '%s\n' $'m!(\047ab\047"\047");' '#[allow(x)]' '// "' '#[allow(x)]')"
put g.rs "$(printf '%s\n' $'m!(\047\\\047"\047);' '#[allow(x)]' '// "')"
put h.rs "$(printf '%s\n' $'m!(\047\\\303\251"\047);' '#[allow(x)]' '// "')"
put i.rs "$(printf '%s\n' $'m!(xb\047a"\047);' '#[allow(x)]' '");' '#[allow(x)]')"
put j.rs "$(printf '%s\n' $'m!(\047(' $'\047"' '#[allow(x)]' '");' '#[allow(x)]')"
put k.rs "$(printf '%s\n' $'m!(\047a\342\200\250\047"\047);' '#[allow(x)]' '// "')"
names "$(printf '%s\n' 'c.rs 2' 'd.rs 4' 'e.rs 4' 'f.rs 2' 'f.rs 4' 'g.rs 2' 'h.rs 2' 'i.rs 4' \
  'j.rs 5' 'k.rs 2')" \
  "a character is told from a lifetime, and read to its close, as rustc reads them"
# Lines 2 and 6 sit in a string after a string's suffix ending r; each
# third line from 10 to 28 follows a raw string after Rust's whitespace past
# ASCII, which ends a name, line 33 one closing at its second hash, and
# line 35 one after a suffix that whitespace ends.
tree
put s.rs "$(printf '%s\n' \
  'm!("a"r"\";' '#[allow(x)]' '");' '#[allow(x)]' \
  'm!(r"a"r"\";' '#[allow(x)]' '");' '#[allow(x)]' \
  $'m!(\302\205r"\\");' '#[allow(x)]' '// ");' \
  $'m!(\342\200\216r"\\");' '#[allow(x)]' '// ");' \
  $'m!(\342\200\217r"\\");' '#[allow(x)]' '// ");' \
  $'m!(\342\200\250r"\\");' '#[allow(x)]' '// ");' \
  $'m!(\342\200\251r"\\");' '#[allow(x)]' '// ");' \
  $'m!(\342\200\250br"\\");' '#[allow(x)]' '// ");' \
  $'m!(r#x\342\200\250r"\\");' '#[allow(x)]' '// ");' \
  'const R: &str = r##"a "# b' '#[allow(x)]' '"##;' '#[allow(x)]' \
  $'m!("a"\342\200\250r"\\");' '#[allow(x)]' '// ");')"
names "$(printf 's.rs %s\n' 4 8 10 13 16 19 22 25 28 33 35)" \
  "a suffix opens no raw string, and Rust's whitespace ends a name"
# A shebang is dropped, past a byte-order mark, and a `#!` opening an
# attribute isn't one.
tree
put t.rs "$(printf '\357\273\277%s\n' '#!/x "'; printf '%s\n' '/* "' '#[allow(x)]' '*/' '#[allow(x)]')"
put u.rs "$(printf '%s\n' '#!/x "' '#[allow(x)]' '// "' '#[allow(x)]')"
put v.rs "$(printf '%s\n' '#! /* a */ [allow(x)]' '#[allow(x)]')"
put w.rs "$(printf '\357\273\277%s\n' '#![allow(x)]')"
names "$(printf '%s\n' 't.rs 5' 'u.rs 2' 'u.rs 4' 'v.rs 1' 'v.rs 2' 'w.rs 1')" \
  "a shebang's quote or comment opens nothing, and a #![ past a byte-order mark is read"
# Past a `#!`, rustc passes only whitespace and plain comments in looking
# for a `[`, so a doc comment or a byte-order mark there makes it a
# shebang, and a comment it leaves open is read on from line 2.
tree
put a.rs "$(printf '%s\n' '#!/// a' '[allow(x)]')"
put b.rs "$(printf '%s\n' '#!//// a' '[allow(x)]')"
put c.rs "$(printf '%s\n' '#!//! a' '[allow(x)]')"
put d.rs "$(printf '%s\n' '#!/** */[ "' '#[allow(x)]' '// "')"
put e.rs '#!/*** */[allow(x)]'
put f.rs '#!/**/[allow(x)]'
put g.rs '#!/*! */[allow(x)]'
put h.rs "$(printf '#!\357\273\277%s\n' '[ "'; printf '%s\n' '#[allow(x)]' '// "')"
put j.rs "$(printf '%s\n' '#!/* a' '#[allow(x)]')"
names "$(printf '%s\n' 'b.rs 1' 'd.rs 2' 'e.rs 1' 'f.rs 1' 'h.rs 2' 'j.rs 2')" \
  "only whitespace and plain comments past a bang keep it an attribute"
tree
put a.rs '#[allow(x)]'
put w.rs 'const S: &str = "a;'
refuses_naming "These leave a comment, string or character open, so their lint-offs can't be \
read as rustc reads them:
w.rs" "in naming lines, one left in a string is refused and no other named"
tree
put w.rs 'const S: &str = "a;'
expect fails $'read as rustc reads them:\nw.rs' "a Rust file left in a string is refused"
tree
put w.rs 'const S: &str = r#"a";'
expect fails $'read as rustc reads them:\nw.rs' "so is one left in a raw string"
tree
put w.rs '/* a /* b */'
expect fails $'read as rustc reads them:\nw.rs' "and one left in a comment"
tree
put w.rs "$(printf '%s\n' $'m!(\047\\x' '#[allow(x)]' $'\047);')"
expect fails $'read as rustc reads them:\nw.rs' \
  "and one whose character a line's end stops, though a quote closes one below"
tree
put w.rs $'m!(\047(/ x\047);'
expect fails $'read as rustc reads them:\nw.rs' "and one whose character a slash stops"
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

tree
put b.py "$(printf '%s\n' 'x = 1' 'y = 2  # noqa: E501' 'z = 3  # noqa: F401')"
put a.ts "$(printf '%s\n' '// @ts-ignore' 'let a = 1;')"
lines c.sh 801
put nix/suppressions-waiting.txt "b.py 1"
names $'a.ts 1\nb.py 2\nb.py 3' \
  "each line turning a linter off is named by its file and line, and nothing is compared"
# Each extension a language's files take is read as that language, as
# code-health reads it: Python's stubs, and JavaScript and TypeScript's
# JSX and module-kind extensions.
tree
put a.pyi "x: int  # type: ignore"
put b.mts "// @ts-expect-error"
put c.cts "// eslint-disable-next-line"
put d.jsx "// biome-ignore lint: it reads a stream"
lines f.mts 801
names $'a.pyi 1\nb.mts 1\nc.cts 1\nd.jsx 1' \
  "a suppression in a .pyi, .mts, .cts or .jsx file is named as its language's"
expect fails $'don\'t say why:\nf.mts' "a long .mts file is counted"
tree
put a.ts "let a = 1;"
names "" "a tree turning nothing off names nothing"
tree
put a.ts "// @ts-ignore"
said=$(cd "${dir}" && GIT_DIR="${git_dir}" "${shell}" "${counts}" --name 2>&1) || code=$?
if [[ ${code:-0} != 64 || ${said} != *"not --name"* ]]; then
  echo "FAIL any other word is refused as asked wrongly: it exits ${code:-0}, saying: ${said}" >&2
  failed=1
fi

if ((failed == 0)); then
  echo "counts-test: every case holds"
fi
exit "${failed}"
