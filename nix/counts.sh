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
#
#   counts.sh [--name-lines]
#
# With --name-lines it compares nothing and says each line it reads as
# turning a linter off, as `PATH LINE`, so the gate can see which lines a
# new reader counts that main's doesn't.
#
# Long because the gate runs main's copy of this file alone, so its Rust
# reader can't live in a file of its own until the gate takes both.
set -euo pipefail

naming=0
case ${1:-} in
  "") ;;
  --name-lines) naming=1 ;;
  *)
    echo "counts.sh: say nothing, or --name-lines, not $1" >&2
    exit 64
    ;;
esac

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
code_pattern='\.(bash|c|cc|cjs|cpp|cts|go|h|js|jsx|lua|mjs|mts|nix|py|pyi|qml|rb|rs|sh|tf|ts|tsx)$'
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

# Whether each line of a Rust file opens with an attribute turning a lint
# off, read as code-health's attributes.rs reads it: from the line on,
# across lines to each attribute's close, past comments, strings and
# characters, and into a `cfg_attr`'s attributes, its condition among
# them. Only a line opening in code is read, not one inside a comment,
# string or character begun above it. A lint turned off part way through
# a line, as past a comment closing on it, isn't seen, as there; one
# named rawly, as `r#allow`, is Rust's own `allow`, so it counts. The file
# is held as its bytes, `rust[1]` to `rust[bytes]`, read from `pos`, and
# walked from its start by `scan`.
read -r -d '' attributes << 'AWK' || true
BEGIN {
  for (n = 1; n < 256; n++) {
    byte[sprintf("%c", n)] = n
  }
}
# The byte at `at`, or nothing past the file's end.
function byte_at(at) {
  return at <= bytes ? rust[at] : ""
}
# How many bytes of Rust's whitespace open at `at`, or of a byte-order
# mark, which opens a file as whitespace would: ASCII's, then U+0085,
# U+200E, U+200F, U+2028 and U+2029, but no no-break space.
function space_at(at,    c, three) {
  c = byte_at(at)
  if (c == "\t" || c == "\n" || c == "\013" || c == "\014" || c == "\r" || c == " ") {
    return 1
  }
  if (c byte_at(at + 1) == "\302\205") {
    return 2
  }
  three = c byte_at(at + 1) byte_at(at + 2)
  if (three == "\342\200\216" || three == "\342\200\217" || three == "\342\200\250" \
    || three == "\342\200\251" || three == "\357\273\277") {
    return 3
  }
  return 0
}
# How many bytes the UTF-8 character opening with byte `c` takes.
function width(c) {
  if (byte[c] >= 240) {
    return 4
  }
  if (byte[c] >= 224) {
    return 3
  }
  return byte[c] >= 192 ? 2 : 1
}
# Passes a comment opening at `pos`, saying whether a line ended inside
# it. A line comment is passed to its line's end; Rust's block comments
# nest.
function pass_comment(    newline, depth) {
  if (byte_at(pos + 1) == "/") {
    while (pos <= bytes && rust[pos] != "\n") {
      pos++
    }
    return 0
  }
  depth = 0
  while (pos <= bytes) {
    if (rust[pos] == "/" && byte_at(pos + 1) == "*") {
      depth++
      pos += 2
    } else if (rust[pos] == "*" && byte_at(pos + 1) == "/") {
      depth--
      pos += 2
      if (depth == 0) {
        return newline
      }
    } else {
      newline = newline || rust[pos] == "\n"
      pos++
    }
  }
  open_end = 1
  return newline
}
# Whether a comment opens at `at`.
function comment_at(at) {
  return byte_at(at) == "/" && (byte_at(at + 1) == "/" || byte_at(at + 1) == "*")
}
# Whether a doc comment opens at `at`: `//!`, `/*!`, or `///` or `/**`
# but for `////`, `/***` and `/**/`, which are plain.
function doc_comment_at(at,    three, fourth) {
  three = byte_at(at) byte_at(at + 1) byte_at(at + 2)
  fourth = byte_at(at + 3)
  return three == "//!" || three == "/*!" || three == "///" && fourth != "/" \
    || three == "/**" && fourth != "*" && fourth != "/"
}
# Passes what rustc passes after a shebang's `#!` in deciding whether it
# is one: Rust's whitespace and plain comments, but no byte-order mark or
# doc comment.
function pass_plain_space(    k) {
  while (1) {
    if (comment_at(pos) && !doc_comment_at(pos)) {
      pass_comment()
    } else if ((k = space_at(pos)) > 0 && rust[pos] != "\357") {
      pos += k
    } else {
      return
    }
  }
}
# Passes whitespace and comments from `pos`, saying whether a line ended
# among them; with `to_line_end`, it stops at the first that does, so a
# long run of comments below a line isn't walked for each.
function pass_space(to_line_end,    newline, k) {
  while (!(to_line_end && newline)) {
    if (comment_at(pos)) {
      newline = pass_comment() || newline
    } else if ((k = space_at(pos)) > 0) {
      newline = newline || rust[pos] == "\n"
      pos += k
    } else {
      return newline
    }
  }
  return newline
}
# Passes a string from its opening quote, escapes and all, and its
# suffix.
function pass_string(    c) {
  pos++
  while (pos <= bytes) {
    c = rust[pos++]
    if (c == "\\") {
      pos++
    } else if (c == "\"") {
      pass_name()
      return
    }
  }
  open_end = 1
}
# Passes the name at `pos`, as a string's suffix, up to the whitespace
# past ASCII that ends it, keeping where it ends in `name_ends`.
function pass_name() {
  while (pos <= bytes && wordy(rust[pos]) && !space_at(pos)) {
    pos++
  }
  name_ends = pos
}
# Passes a raw string from its `r`, saying whether one stood there.
function pass_raw_string(    hashes, from, k, closes) {
  hashes = 0
  while (byte_at(pos + 1 + hashes) == "#") {
    hashes++
  }
  if (byte_at(pos + 1 + hashes) != "\"") {
    return 0
  }
  for (from = pos + 2 + hashes; from <= bytes; from++) {
    closes = rust[from] == "\""
    for (k = 1; closes && k <= hashes; k++) {
      closes = byte_at(from + k) == "#"
    }
    if (closes) {
      pos = from + 1 + hashes
      pass_name()
      return 1
    }
  }
  pos = bytes + 1
  open_end = 1
  return 1
}
# Passes a character from its opening quote, and its suffix, saying
# whether one stood there rather than a lifetime, as rustc's
# lifetime_or_char tells them apart. A quote before a letter, digit or `_`,
# or any character past ASCII, opens a lifetime unless a quote follows
# that one character or the name it opens, which closes a character. A
# byte character's quote, after a `b` no name runs into and ending no
# suffix or raw name, opens no lifetime. Any other is read as rustc's
# single_quoted_string reads it, each escape and what it escapes as one,
# to its close; one left open at a `/`, at a line's end no quote follows,
# or at the file's end is an open end, read on from there.
function pass_character(    at) {
  if (pos + 1 <= bytes && rust[pos + 1] != "\\" \
    && byte_at(pos + 1 + width(rust[pos + 1])) == "\047") {
    pos += width(rust[pos + 1]) + 2
    pass_name()
    return 1
  }
  if (pos + 1 <= bytes && wordy(rust[pos + 1]) && !opens_byte(pos)) {
    at = pos + 1 + width(rust[pos + 1])
    while (at <= bytes && wordy(rust[at]) && !space_at(at)) {
      at++
    }
    if (byte_at(at) != "\047") {
      return 0
    }
    pos = at + 1
    return 1
  }
  for (at = pos + 1; at <= bytes; at++) {
    if (rust[at] == "\047") {
      pos = at + 1
      pass_name()
      return 1
    }
    if (rust[at] == "/" || rust[at] == "\n" && byte_at(at + 1) != "\047") {
      break
    }
    if (rust[at] == "\\") {
      at++
    }
  }
  pos = at
  open_end = 1
  return 1
}
# Whether the quote at `at` opens a byte character: a `b` no name runs
# into stands right before it, as rustc's c_or_byte_string reads one, and
# ends no suffix or raw name, which rustc has read whole.
function opens_byte(at) {
  return name_ends != at && byte_at(at - 1) == "b" && !ends_a_name(at - 2)
}
# Whether byte `c` may be part of a name: an ASCII letter, digit or
# underscore, or any byte of a wider character.
function wordy(c) {
  return c ~ /[A-Za-z0-9_]/ || byte[c] >= 128
}
# Passes a raw name, `r#` and a name, from `pos`, saying whether one
# stood there, so its last letter isn't read as an `r` opening a raw
# string; `raw_name` holds what it was. One a name runs into is none:
# its `r` ends a number's suffix.
function pass_raw_name() {
  if (byte_at(pos) != "r" || byte_at(pos + 1) != "#" || !wordy(byte_at(pos + 2)) \
    || ends_a_name(pos - 1)) {
    return 0
  }
  raw_name = "r#"
  for (pos += 2; pos <= bytes && wordy(rust[pos]) && !space_at(pos); pos++) {
    raw_name = raw_name rust[pos]
  }
  name_ends = pos
  return 1
}
# Whether the `r` at `at` may open a raw string: no name or lifetime runs
# into it, but for a byte or C string's `b` or `c`, since Rust reserves
# any other name written right against a string.
function opens_raw(at,    before, far) {
  before = at > 1 ? rust[at - 1] : ""
  if (!ends_a_name(at - 1) && before != "\047") {
    return 1
  }
  far = at > 2 ? rust[at - 2] : ""
  return (before == "b" || before == "c") && !ends_a_name(at - 2) && far != "\047"
}
# Whether the byte at `at` ends a name: Rust's whitespace past ASCII ends
# none, nor does the file's start.
function ends_a_name(at) {
  if (at < 1 || at >= 2 && space_at(at - 1) == 2 || at >= 3 && space_at(at - 2) == 3) {
    return 0
  }
  return wordy(rust[at])
}
# The words inside the next attribute's brackets, where one opens before
# the line ends, with whitespace and comments gone and each string or
# character as its quotes alone; `found` says whether one did.
function attribute(    c, inner, depth) {
  found = 0
  if (pass_space(1) || pos > bytes || rust[pos++] != "#") {
    return ""
  }
  pass_space()
  if (byte_at(pos) == "!") {
    pos++
    pass_space()
  }
  if (pos > bytes || rust[pos++] != "[") {
    return ""
  }
  inner = ""
  depth = 0
  while (1) {
    pass_space()
    if (pos > bytes) {
      return ""
    }
    c = rust[pos]
    if (pass_raw_name()) {
      inner = inner substr(raw_name, 3)
    } else if (c == "\"") {
      pass_string()
      inner = inner "\"\""
    } else if (c == "r" && opens_raw(pos) && pass_raw_string()) {
      inner = inner "\"\""
    } else if (c == "\047" && pass_character()) {
      inner = inner "\047\047"
    } else if (c == "]" && depth == 0) {
      pos++
      found = 1
      return inner
    } else {
      if (c == "[") {
        depth++
      } else if (c == "]") {
        depth--
      }
      inner = inner c
      pos++
    }
  }
}
# Whether an attribute's words turn a lint off: `allow` or `expect`, or a
# `cfg_attr` holding one among its attributes, each read so, split only at
# commas outside any of their own brackets, of whatever shape.
function holds_a_lint_off(inner,    given, depth, from, at, c, count, part, n) {
  if (index(inner, "allow(") == 1 || index(inner, "expect(") == 1) {
    return 1
  }
  if (index(inner, "cfg_attr(") != 1) {
    return 0
  }
  given = substr(inner, 10)
  depth = 0
  from = 1
  count = 0
  for (at = 1; at <= length(given); at++) {
    c = substr(given, at, 1)
    if (c == "(" || c == "[" || c == "{") {
      depth++
    } else if (c == ")" && depth == 0) {
      part[++count] = substr(given, from, at - from)
      break
    } else if ((c == ")" || c == "]" || c == "}") && depth > 0) {
      depth--
    } else if (c == "," && depth == 0) {
      part[++count] = substr(given, from, at - from)
      from = at + 1
    }
  }
  for (n = 1; n <= count; n++) {
    if (holds_a_lint_off(part[n])) {
      return 1
    }
  }
  return 0
}
# Whether the line opening at `start` opens with an attribute turning a
# lint off, past any that don't.
function turns_a_lint_off(start,    inner) {
  pos = start
  while (1) {
    inner = attribute()
    if (!found) {
      return 0
    }
    if (holds_a_lint_off(inner)) {
      return 1
    }
  }
}
# Moves `scan` on to `start`, past the code between, saying whether
# `start` lies in code rather than inside a comment, string or character
# opened before it.
function in_code(start) {
  while (scan < start) {
    pos = scan
    if (comment_at(scan)) {
      pass_comment()
    } else if (rust[scan] == "\"") {
      pass_string()
    } else if (!pass_raw_name() && !(rust[scan] == "r" && opens_raw(scan) && pass_raw_string()) \
      && !(rust[scan] == "\047" && pass_character())) {
      pos++
    }
    scan = pos
  }
  return scan == start
}
# Each line of the Rust file held turning a lint off, as `suppression
# PATH LINE`, from those opening in code; or, where a comment, string or
# raw string is left open at its end, or a character where rustc stops
# it, `unread PATH`, since the walk may have read it otherwise than rustc
# would. A first line opening `#!`, past a
# byte-order mark, is a shebang rustc drops, unless what follows past
# whitespace and plain comments is a `[`. A comment left open there is
# no open end, since rustc reads on from line 2.
function rust_ended(    start, line) {
  start = 1
  line = 1
  scan = 1
  open_end = 0
  name_ends = 0
  pos = (rust[1] rust[2] rust[3]) == "\357\273\277" ? 4 : 1
  if (byte_at(pos) == "#" && byte_at(pos + 1) == "!") {
    pos += 2
    pass_plain_space()
    open_end = 0
    if (byte_at(pos) != "[") {
      for (scan = 1; scan <= bytes && rust[scan] != "\n"; scan++) {
      }
    }
  }
  while (start <= bytes) {
    if (in_code(start) && turns_a_lint_off(start)) {
      print "suppression " file " " line
    }
    while (start <= bytes && rust[start] != "\n") {
      start++
    }
    start++
    line++
  }
  in_code(bytes + 1)
  if (open_end) {
    print "unread " file
  }
}
AWK

# Each code file past 800 lines that doesn't say why, as `long PATH`, and
# each line turning a linter off, as `suppression PATH LINE`.
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
function suppresses(path, line,    extension, terraform) {
  extension = path
  sub(/^.*\./, "", extension)
  if (extension == "py" || extension == "pyi") {
    return comment(line, "#", "noqa|type: ignore|pyright: ignore|pylint: disable")
  }
  if (extension == "sh" || extension == "bash") {
    return comment(line, "#", "shellcheck disable")
  }
  if (extension == "qml") {
    return comment(line, "//", "qmllint disable")
  }
  if (extension ~ /^(js|mjs|cjs|jsx|ts|mts|cts|tsx)$/) {
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
  if (file != "" && in_rust) {
    rust_ended()
  }
}
FNR == 1 {
  ended()
  file = substr(FILENAME, 3)
  lines = 0
  said = 0
  in_rust = file ~ /\.rs$/
  bytes = 0
}
{
  lines = FNR
  if (FNR <= 40 && opens_sentence($0, "Long because")) {
    said = 1
  }
  # A Rust file is read whole once it's held, since an attribute runs on
  # across lines.
  if (in_rust) {
    count = split($0, part, "")
    for (n = 1; n <= count; n++) {
      rust[++bytes] = part[n]
    }
    rust[++bytes] = "\n"
  } else if (suppresses(file, $0)) {
    print "suppression " file " " FNR
  }
}
END {
  ended()
}
AWK
measured=""
if ((${#code[@]} > 0)); then
  measured="$(awk "${characters}
${attributes}
${measure}" "${code[@]}")"
fi
unread="$(sed -n 's/^unread //p' <<< "${measured}")"
if [[ -n ${unread} ]]; then
  echo "These leave a comment, string or character open, so their lint-offs can't be" \
    "read as rustc reads them:" >&2
  echo "${unread}" >&2
  exit 1
fi
if ((naming)); then
  sed -n 's/^suppression //p' <<< "${measured}" | sort -k 1,1 -k 2,2n
  exit 0
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
suppression_found="$(awk 'sub(/^suppression /, "") { sub(/ [0-9]+$/, ""); found[$0]++ }
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
