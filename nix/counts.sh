#!/usr/bin/env bash
# The counts that only fall, read as core.nix's code-health reads them each
# hour (its measure.rs), so the hour's figure and this check agree: a code
# file past 800 lines whose first 40 don't say "Long because", a folder of
# more than 40 entries whose README.md or AGENTS.md doesn't say "Many
# entries because", and each line that turns a linter off. Those there
# before the rule wait in nix/long-files-waiting.txt and
# nix/suppressions-waiting.txt, and this fails on one that isn't listed and
# on a line listed past what's found, so the lists only shrink. Run from
# the repository's root; scripts/merge-gate.sh runs main's copy over each
# head it merges, with whatever bash and awk the machine has, so this keeps
# to what a Mac's bash 3.2 and its awk do too.
set -euo pipefail

# Bytes, whatever the machine's locale, so every awk reads a file alike
# and none stops at a byte its locale can't read. Rust's characters are
# matched as their UTF-8 bytes below.
export LC_ALL=C

long_list=nix/long-files-waiting.txt
suppression_list=nix/suppressions-waiting.txt

# The files git keeps or would keep, as nix/tree.sh reads them; in a copy
# of the tree with no git, every file but links, found as a Mac's find
# finds them too, since the gate may run there.
if git rev-parse --git-dir > /dev/null 2>&1; then
  listed="$(git -c core.quotePath=false ls-files --cached --others --exclude-standard)"
else
  listed="$(find . -type f | sed 's|^\./||')"
fi
sorted="$(sort <<< "${listed}")"

# The code files, by code-health's extensions, less what's generated.
code_pattern='\.(bash|c|cc|cjs|cpp|go|h|js|lua|mjs|nix|py|qml|rb|rs|sh|tf|ts|tsx)$'
coded="$(grep -E "${code_pattern}" <<< "${sorted}" \
  | grep -Ev '(^|/)node_modules/|\.min\.js$' || true)"

# A list names a file and its count apart by a space, and git quotes a
# path holding a tab, a newline, a quote or a backslash, so a code file
# whose path holds either can't be counted or listed.
unnamed="$(grep -E '[[:space:]]|^"' <<< "${coded}" || true)"
if [[ -n ${unnamed} ]]; then
  echo "These code files' paths hold a space or a character git quotes," \
    "so the lists can't name them:" >&2
  printf '%s\n' "${unnamed}" >&2
  echo "Rename them." >&2
  exit 1
fi

# Each as ./PATH, so awk reads none as an assignment, as it would a=b.sh.
code=()
while IFS= read -r file; do
  if [[ -n ${file} ]]; then
    code+=("./${file}")
  fi
done <<< "${coded}"
notes=()
while IFS= read -r file; do
  if [[ -n ${file} ]]; then
    notes+=("./${file}")
  fi
done <<< "$(grep -E '(^|/)(README|AGENTS)\.md$' <<< "${sorted}" || true)"

# Rust's own whitespace and letters, as measure.rs's trim_start, trim_end,
# split_whitespace and is_alphanumeric read them, for both programs below.
read -r -d '' characters << 'AWK' || true
BEGIN {
  # char::is_whitespace: ASCII's, then U+0085, U+00A0, U+1680, U+2000 to
  # U+200A, U+2028, U+2029, U+202F, U+205F and U+3000.
  space = "[\t\n\013\014\r ]|\302\205|\302\240|\341\232\200|\342\200\250|\342\200\251"
  space = space "|\342\200\257|\342\201\237|\343\200\200"
  for (n = 128; n <= 138; n++) {
    space = space "|\342\200" sprintf("%c", n)
  }
  for (n = 1; n < 128; n++) {
    ascii = ascii sprintf("%c", n)
  }
  # A UTF-8 character's later bytes.
  for (n = 128; n < 192; n++) {
    later = later sprintf("%c", n)
  }
  # What isn't a letter or digit in Latin-1 (U+0080 to U+00BF, then x and
  # its divide), and in general punctuation, U+2040 to U+206F.
  for (n = 128; n < 192; n++) {
    if (n != 170 && n != 178 && n != 179 && n != 181 && n != 185 && n != 186 \
      && n != 188 && n != 189 && n != 190) {
      latin = latin sprintf("%c", n)
    }
  }
  for (n = 128; n < 176; n++) {
    punctuation = punctuation sprintf("%c", n)
  }
}
function trim_start(text) {
  while (match(text, "^(" space ")")) {
    text = substr(text, RLENGTH + 1)
  }
  return text
}
function trim_end(text) {
  while (match(text, "(" space ")$")) {
    text = substr(text, 1, RSTART - 1)
  }
  return text
}
# Whether the character opening `text` is a letter or a digit. Past ASCII,
# Latin-1 and general punctuation are read exactly; every other character
# counts as one, as most that follow a word directly are.
function alphanumeric(text,    first, second) {
  first = substr(text, 1, 1)
  second = substr(text, 2, 1)
  if (first == "") {
    return 0
  }
  if (index(ascii, first)) {
    return first ~ /[[:alnum:]]/
  }
  if (match(text, "^(" space ")")) {
    return 0
  }
  if (first == "\302") {
    return !index(latin, second)
  }
  if (first == "\303") {
    return second != "\227" && second != "\267"
  }
  if (first == "\342" && second == "\200") {
    return 0
  }
  if (first == "\342" && second == "\201") {
    return !index(punctuation, substr(text, 3, 1))
  }
  return 1
}
# The last character of `text`, its later bytes and all.
function last(text,    at) {
  at = length(text)
  while (at > 1 && index(later, substr(text, at, 1))) {
    at--
  }
  return substr(text, at)
}
AWK

# Each code file past 800 lines that doesn't say why, as `long PATH`, and
# each line turning a linter off, as `suppression PATH`, one for each.
# A mark opens a comment only where no quote before it is left open, and
# the next is looked for past the whole of this one, as Rust's
# match_indices steps.
read -r -d '' measure << 'AWK' || true
function opens_sentence(line, words,    rest, at, offset, before, after) {
  rest = line
  offset = 0
  while ((at = index(rest, words)) > 0) {
    before = trim_end(substr(line, 1, offset + at - 1))
    after = substr(line, offset + at + length(words), 4)
    if ((before == "" || before ~ /[.:;!?]$/ || before ~ /^[#\/*!; -]*$/) \
      && !alphanumeric(after)) {
      return 1
    }
    offset += at + length(words) - 1
    rest = substr(line, offset + 1)
  }
  return 0
}
function quotes_open(before,    doubles, singles) {
  doubles = gsub(/"/, "", before)
  singles = gsub(/\047/, "", before)
  return doubles % 2 == 1 || singles % 2 == 1
}
function comment(line, mark, markers,    rest, at, offset, said, n, i, list) {
  n = split(markers, list, "|")
  rest = line
  offset = 0
  while ((at = index(rest, mark)) > 0) {
    said = trim_start(substr(line, offset + at + length(mark)))
    if (!quotes_open(substr(line, 1, offset + at - 1))) {
      for (i = 1; i <= n; i++) {
        if (index(said, list[i]) == 1) {
          return 1
        }
      }
    }
    offset += at + length(mark) - 1
    rest = substr(line, offset + 1)
  }
  return 0
}
function suppresses(path, line,    extension, trimmed, terraform) {
  extension = path
  sub(/^.*\./, "", extension)
  if (extension == "rs") {
    trimmed = trim_start(line)
    return index(trimmed, "#[allow(") == 1 || index(trimmed, "#![allow(") == 1 \
      || index(trimmed, "#[expect(") == 1 || index(trimmed, "#![expect(") == 1
  }
  if (extension == "py") {
    return comment(line, "#", "noqa|type: ignore|pyright: ignore|pylint: disable")
  }
  if (extension == "sh" || extension == "bash") {
    return comment(line, "#", "shellcheck disable")
  }
  if (extension == "qml") {
    return comment(line, "//", "qmllint disable")
  }
  if (extension ~ /^(js|mjs|cjs|ts|tsx)$/) {
    return comment(line, "//", \
      "biome-ignore|eslint-disable|@ts-ignore|@ts-expect-error|@ts-nocheck")
  }
  if (extension == "nix") {
    return comment(line, "#", "deadnix: skip")
  }
  if (extension == "tf") {
    terraform = "tflint-ignore|trivy:ignore|tfsec:ignore"
    return comment(line, "#", terraform) || comment(line, "//", terraform)
  }
  return 0
}
function ended() {
  if (file != "" && lines > 800 && !said) {
    print "long " file
  }
}
FNR == 1 {
  ended()
  file = substr(FILENAME, 3)
  lines = 0
  said = 0
}
{
  lines = FNR
  if (FNR <= 40 && opens_sentence($0, "Long because")) {
    said = 1
  }
  if (suppresses(file, $0)) {
    print "suppression " file
  }
}
END {
  ended()
}
AWK
measured=""
if ((${#code[@]} > 0)); then
  measured="$(awk "${characters}
${measure}" "${code[@]}")"
fi

# Each folder of more than 40 entries, the files and folders right in it
# counted alike, as `crowded FOLDER`, the top one as `.`.
read -r -d '' entries << 'AWK' || true
{
  path = $0
  while (match(path, /\/[^\/]*$/)) {
    folder = substr(path, 1, RSTART - 1)
    inside[folder SUBSEP path] = 1
    path = folder
  }
  inside["." SUBSEP path] = 1
}
END {
  for (key in inside) {
    split(key, part, SUBSEP)
    count[part[1]]++
  }
  for (folder in count) {
    if (count[folder] > 40) {
      print "crowded " folder
    }
  }
}
AWK
crowded="$(awk "${entries}" <<< "${sorted}")"

# Each folder whose note says why, its words joined across lines and each
# whole, as `reasoned FOLDER`.
read -r -d '' mentions << 'AWK' || true
function said(text,    words, rest, at, offset, before, after) {
  words = "Many entries because"
  gsub("(" space ")+", " ", text)
  rest = text
  offset = 0
  while ((at = index(rest, words)) > 0) {
    before = last(substr(text, 1, offset + at - 1))
    after = substr(text, offset + at + length(words), 4)
    if (!(before == "_" || alphanumeric(before)) \
      && !(substr(after, 1, 1) == "_" || alphanumeric(after))) {
      return 1
    }
    offset += at + length(words) - 1
    rest = substr(text, offset + 1)
  }
  return 0
}
function ended(    folder) {
  if (file != "" && said(text)) {
    folder = file
    if (!sub(/\/[^\/]*$/, "", folder)) {
      folder = "."
    }
    print "reasoned " folder
  }
}
FNR == 1 {
  ended()
  file = substr(FILENAME, 3)
  text = ""
}
{
  text = text $0 "\n"
}
END {
  ended()
}
AWK
reasoned=""
if ((${#notes[@]} > 0)); then
  reasoned="$(awk "${characters}
${mentions}" "${notes[@]}" | sed -n 's/^reasoned //p')"
fi

# What's found now: long files and crowded folders, a path a line, and each
# file's suppressions, as `PATH COUNT`.
long_found="$(
  {
    sed -n 's/^long //p' <<< "${measured}"
    grep -Fvx -f <(printf '%s\n' "${reasoned}" | sed '/^$/d; s/^/crowded /') \
      <<< "${crowded}" | sed -n 's/^crowded //p' || true
  } | sed '/^$/d' | sort -u
)"
suppression_found="$(awk 'sub(/^suppression /, "") { found[$0]++ }
  END { for (file in found) print file, found[file] }' <<< "${measured}" | sort)"

# A list's lines but its comments and blanks.
listed_lines() {
  if [[ -e $1 ]]; then
    grep -Ev '^[[:space:]]*(#|$)' "$1" || true
  fi
}

failed=0
# A line with a space at either end would name another path than its
# trimmed form, which is how the gate keys it, so neither list may hold one.
long_lines="$(listed_lines "${long_list}")"
suppression_lines="$(listed_lines "${suppression_list}")"
padded="$(grep -E '^[[:space:]]|[[:space:]]$' <<< "${long_lines}"$'\n'"${suppression_lines}" \
  || true)"
if [[ -n ${padded} ]]; then
  echo "These lines of the lists begin or end with a space; write each without:" >&2
  printf '%s\n' "${padded}" >&2
  failed=1
fi
long_listed="$(listed_lines "${long_list}" | sort -u)"
unlisted="$(comm -23 <(printf '%s\n' "${long_found}") <(printf '%s\n' "${long_listed}"))"
if [[ -n ${unlisted} ]]; then
  echo "These are past 800 lines or 40 entries and don't say why:" >&2
  printf '%s\n' "${unlisted}" >&2
  echo "Split them, or say why in the file's opening comment (\"Long because\")" \
    "or the folder's README.md (\"Many entries because\")." >&2
  failed=1
fi
gone="$(comm -13 <(printf '%s\n' "${long_found}") <(printf '%s\n' "${long_listed}"))"
if [[ -n ${gone} ]]; then
  echo "These wait in ${long_list} but are no longer past the line or say why:" >&2
  printf '%s\n' "${gone}" >&2
  echo "Take their lines off the list." >&2
  failed=1
fi

suppression_listed="$(listed_lines "${suppression_list}")"
unread="$(grep -Evx '[^[:space:]]+[[:space:]]+[1-9][0-9]*' <<< "${suppression_listed}" || true)"
if [[ -n ${unread} ]]; then
  echo "These lines of ${suppression_list} aren't a file and a count above 0:" >&2
  printf '%s\n' "${unread}" >&2
  failed=1
fi
said_counts="$(
  awk 'FNR == NR { if (NF) found[$1] = $2; next }
    NF { listed[$1] = $2 }
    END {
      for (file in found) {
        if (found[file] > listed[file] + 0) {
          print "raised " file ": " found[file] ", listed " listed[file] + 0
        }
      }
      for (file in listed) {
        if (found[file] + 0 < listed[file] + 0) {
          print "fell " file " " found[file] + 0 " (listed " listed[file] ")"
        }
      }
    }' <(printf '%s\n' "${suppression_found}") <(printf '%s\n' "${suppression_listed}") | sort
)"
raised="$(sed -n 's/^raised //p' <<< "${said_counts}")"
if [[ -n ${raised} ]]; then
  echo "These turn a linter off more often than ${suppression_list} allows:" >&2
  printf '%s\n' "${raised}" >&2
  echo "Fix the code so the linter passes it, rather than turning the lint off." >&2
  failed=1
fi
fell="$(sed -n 's/^fell //p' <<< "${said_counts}")"
if [[ -n ${fell} ]]; then
  echo "These turn a linter off less often than ${suppression_list} says:" >&2
  printf '%s\n' "${fell}" >&2
  echo "Lower their counts, or take their lines off at 0." >&2
  failed=1
fi

exit "${failed}"
