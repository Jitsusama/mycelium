#!/usr/bin/env bash
# The merge gate's cases for a rise in what waits, in a repository of its
# own made here: a head bringing a sharper reader, which sees lints main's
# misses in files main already holds, lets a Rust file main holds rise by
# the lines that reader names and main's doesn't, and nothing else rises.
#
#   merge-gate-rise-test.sh GATE
set -euo pipefail

usage="merge-gate-rise-test.sh GATE"
gate=$(realpath "${1:?Name the gate: ${usage}}")
# shellcheck source=scripts/gate-test-kit.sh
source "$(dirname "${BASH_SOURCE[0]}")/gate-test-kit.sh"

# Main's reader here is a dull stand-in: it counts a Rust line only where
# it opens `#[allow(` or `#[expect(`, so lint-offs it misses can play what
# a sharper reader finds, whichever counts.sh the repository holds. A
# stand-in nix answers the gate's build of counts-name-their-lines with the
# lines in ${named_file}, as the head's reader would name them in the
# sandbox, and keeps what it was asked; a stand-in timeout keeps what it
# was asked too, and runs the machine's own. src/split.rs holds two lint-offs
# the dull reader misses, and src/old.rs one beside one it counts.
git init -q --bare -b main "${work}/sharper.git"
git clone -q "${work}/sharper.git" "${work}/sharper" 2> /dev/null
cd "${work}/sharper"
mkdir nix src
cat > "${work}/dull-counts.sh" << 'EOF'
# A dull reader: a Rust line counts only where it opens #[allow( or
# #[expect(, and nothing else is measured. As the real one does, it
# refuses a listed line that isn't a file and a count above 0, and a file
# counted past its listing or short of it.
set -euo pipefail
found=$(grep -rnE --include='*.rs' '^[[:space:]]*#\[(allow|expect)\(' . \
  | awk -F: '{ sub(/^\.\//, "", $1); print $1 " " $2 }' | sort -k 1,1 -k 2,2n || true)
if [[ ${1:-} == --name-lines ]]; then
  sed '/^$/d' <<< "${found}"
  exit 0
fi
unread=$(awk '!/^#/ && NF && (NF != 2 || $2 !~ /^[1-9][0-9]*$/)' nix/suppressions-waiting.txt)
if [[ -n ${unread} ]]; then
  echo "These lines in nix/suppressions-waiting.txt aren't a file and a count above 0:"
  echo "${unread}"
  exit 1
fi
over=$(awk '
  FNR == NR { if ($0 !~ /^#/ && NF) listed[$1] = $2; next }
  NF { n[$1]++ }
  END { for (f in n) if (n[f] > listed[f] + 0) print f ": " n[f] ", listed " listed[f] + 0 }
' nix/suppressions-waiting.txt <(printf '%s\n' "${found}"))
under=$(awk '
  FNR == NR { if ($0 !~ /^#/ && NF) listed[$1] = $2; next }
  NF { n[$1]++ }
  END { for (f in listed) if (n[f] + 0 < listed[f]) print f ": " n[f] + 0 ", listed " listed[f] }
' nix/suppressions-waiting.txt <(printf '%s\n' "${found}"))
if [[ -n ${over} ]]; then
  echo "These turn a linter off more often than nix/suppressions-waiting.txt allows:"
  echo "${over}"
fi
if [[ -n ${under} ]]; then
  echo "These turn a linter off less often than nix/suppressions-waiting.txt says:"
  echo "${under}"
fi
if [[ -n ${over}${under} ]]; then exit 1; fi
EOF
cp "${work}/dull-counts.sh" nix/counts.sh
attribute='#[cfg_attr(test, allow(x))]'
printf '%s\n' "${attribute}" 'fn split() {}' "${attribute}" 'fn apart() {}' > src/split.rs
printf '%s\n' '#![ expect(unused) ]' '#[allow(dead_code)]' 'fn old() {}' > src/old.rs
printf '%s\n' 'echo hi' '# allow(nothing) is just a comment' > src/note.sh
mkdir src/dir.rs
echo hi > src/dir.rs/a.txt
ln -s split.rs src/link.rs
printf '%s\n' "${attribute}" > $'src/\303\251.rs'
printf '%s\n' "${attribute}" > ':a.rs'
printf '%s\n' "# Waiting." "src/old.rs 1" > nix/suppressions-waiting.txt
git add nix src ./:a.rs
sharp_main=$(commit sharp)
sharp_top=$(git rev-parse --show-toplevel)
git push -q origin HEAD:main
mkdir "${work}/fake-nix"
named_file="${work}/named"
asked_file="${work}/asked"
cat > "${work}/fake-nix/nix" << EOF
#!${BASH}
printf '%s\n' "\$*" > "${asked_file}"
if [[ -n \${NIX_FAILS:-} ]]; then echo "error: the build failed" >&2; exit 1; fi
if [[ -n \${NIX_EXITS:-} ]]; then exit "\${NIX_EXITS}"; fi
if [[ -n \${NIX_KILLED:-} ]]; then kill -KILL "\$\$"; fi
if [[ -n \${NIX_HANGS:-} ]]; then exec sleep 30; fi
# A child that outlives the stand-in, holding only the build's output, as
# one holding its errors holds whoever reads the gate's to their end.
if [[ -n \${NIX_HOLDS:-} ]]; then sleep 30 2> /dev/null; fi
if [[ -n \${NIX_LINKS:-} ]]; then
  ln -sf "${named_file}" "${work}/linked"
  echo "${work}/linked"
  exit 0
fi
if [[ -n \${NIX_FOLDER:-} ]]; then echo "${work}"; exit 0; fi
echo "${named_file}"
EOF
chmod +x "${work}/fake-nix/nix"
timed_file="${work}/timed"
if ! machines_timeout=$(command -v timeout); then
  echo "merge-gate-rise-test: coreutils' timeout must be on the path, as the gate needs it" >&2
  exit 1
fi
cat > "${work}/fake-nix/timeout" << EOF
#!${BASH}
printf '%s\n' "\$*" > "${timed_file}"
exec ${machines_timeout} "\$@"
EOF
chmod +x "${work}/fake-nix/timeout"
# The head's reader names its lines only in its build's sandbox, which the
# stand-in nix plays, so one run anywhere else leaves its mark.
ran_outside="${work}/ran-outside"
sharper_reader="if [[ \${1:-} == --name-lines ]]; then touch '${ran_outside}'; fi"$'\n'
sharper_reader+="$(cat "${work}/dull-counts.sh")"
# A head off this main holding each `path=text` named, with every check
# passed, its reader named as `named` says.
sharpened() {
  git checkout -q -B sharpened "${sharp_main}"
  local pair
  for pair in "$@"; do
    printf '%s\n' "${pair#*=}" > "${pair%%=*}"
    git add "${pair%%=*}"
  done
  git commit -q -m sharpened
  local sha
  sha=$(git rev-parse HEAD)
  pass "${sha}"
  echo "${sha}"
}
# Whether the gate let `sha` merge or refused it, saying `words`, with
# the head's reader naming `named`.
sharp() {
  local named=$1
  shift
  printf '%s\n' "${named}" > "${named_file}"
  rm -f "${asked_file}"
  PATH="${work}/fake-nix:${PATH}" expect "$@"
}
split_named=$'src/split.rs 1\nsrc/split.rs 3'
raise="nix/suppressions-waiting.txt=# Waiting."$'\nsrc/old.rs 1\nsrc/split.rs 2'
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${raise}")
sharp "${split_named}" merges "${sha}" \
  "lets src/split.rs rise from 0 to 2: ${sha:0:12}'s reader counts lines 1 3 there" \
  "a sharper reader lets a file left as main has it rise by the lines it names"
asked=$(< "${asked_file}")
want="git+file://${sharp_top}?rev=${sha}#checks.x86_64-linux.counts-name-their-lines"
if [[ ${asked} != "build --no-link --print-out-paths --option accept-flake-config false"* ]] \
  || [[ ${asked} != *" ${want}" ]]; then
  echo "FAIL the head's lines come from its check at that commit, on this machine's" \
    "settings alone: asked ${asked}" >&2
  failed=1
fi
timed=none
if [[ -f ${timed_file} ]]; then
  timed=$(< "${timed_file}")
fi
if [[ ${timed} != "--foreground --kill-after=60 1800 nix build "* ]]; then
  echo "FAIL the build is bounded in the gate's own process group, and killed if it" \
    "won't stop: timed ${timed}" >&2
  failed=1
fi
# A folder of every program on the stand-ins' path but one.
without() {
  local dir tool name dirs
  mkdir "${work}/without-$1"
  IFS=: read -r -a dirs <<< "${work}/fake-nix:${PATH}"
  for dir in "${dirs[@]}"; do
    for tool in "${dir}"/*; do
      name=${tool##*/}
      if [[ ${name} != "$1" && -f ${tool} && -x ${tool} && ! -e ${work}/without-$1/${name} ]]
      then
        ln -s "${tool}" "${work}/without-$1/${name}"
      fi
    done
  done
}
without timeout
without nix
printf '%s\n' "${split_named}" > "${named_file}"
PATH="${work}/without-timeout" expect refuses "${sha}" \
  "no timeout command is on this machine's path" \
  "a machine with no timeout to bound the build says so, and builds nothing"
PATH="${work}/without-nix" expect refuses "${sha}" "no nix command is on this machine's path" \
  "nor does one with no nix to build it"
pass "${sha}" mycelium
printf '%s\n' "${split_named}" > "${named_file}"
rm -f "${asked_file}"
said=$(PATH="${work}/fake-nix:${PATH}" bash "${gate}" "${sha}" 2>&1 || true)
if [[ ${said} != *"checks.x86_64-linux.mycelium"* || ${said} == *"lets src/split.rs"* ]]; then
  echo "FAIL a rise on a head lacking a check isn't said as let through: ${said}" >&2
  failed=1
fi
if [[ -e ${asked_file} ]]; then
  echo "FAIL a head lacking any kept check builds nothing: asked $(< "${asked_file}")" >&2
  failed=1
fi
pass "${sha}"
NIX_LINKS=1 sharp "src/split.rs 1" refuses "${sha}" "counts-name-their-lines wrote no file" \
  "the head's lines are read only from a file its build wrote, never through a link"
NIX_FOLDER=1 sharp "src/split.rs 1" refuses "${sha}" "counts-name-their-lines wrote no file" \
  "nor from a folder it names"
MERGE_GATE_BUILD_SECONDS=1 NIX_HANGS=1 sharp "${split_named}" refuses "${sha}" \
  "reader was still building after 1 seconds" "a build that runs past its limit stops the gate"
started=${SECONDS}
MERGE_GATE_BUILD_SECONDS=1 NIX_HOLDS=1 sharp "${split_named}" refuses "${sha}" \
  "reader was still building after 1 seconds" \
  "as does one whose stray child still holds its output"
if ((SECONDS - started >= 20)); then
  echo "FAIL the gate waits on nothing past its limit: it took $((SECONDS - started)) seconds" >&2
  failed=1
fi
killed="reader was killed, by timeout a minute past 1800 seconds or by this machine,"
killed+=" so its rise can't be judged: see whether this machine ran short of memory"
NIX_EXITS=137 sharp "${split_named}" refuses "${sha}" "${killed}" \
  "a build killed is said as killed, by timeout or the machine"
NIX_KILLED=1 sharp "${split_named}" refuses "${sha}" "${killed}" \
  "as is one this machine kills, since timeout passes on its signal"
NIX_EXITS=125 sharp "${split_named}" refuses "${sha}" \
  "this machine couldn't start the build of ${sha:0:12}'s reader (status 125)" \
  "a build this machine can't start is said as the machine's"
NIX_EXITS=126 sharp "${split_named}" refuses "${sha}" "(status 126), so its rise can't be" \
  "as is one it can't run"
NIX_EXITS=127 sharp "${split_named}" refuses "${sha}" "(status 127), so its rise can't be" \
  "or can't find"
MERGE_GATE_BUILD_SECONDS=0 sharp "${split_named}" refuses "${sha}" \
  "MERGE_GATE_BUILD_SECONDS is 0, not a count of seconds above 0" \
  "a limit that bounds nothing is refused"
pass "${sha}" counts-name-their-lines
sharp "src/split.rs 1" refuses "${sha}" \
  "passed at ${sha:0:12}: checks.x86_64-linux.counts-name-their-lines. Build them" \
  "the head's reader is built only once the keeper kept every check as passed there"
if [[ -e ${asked_file} ]]; then
  echo "FAIL a head whose reader's check wasn't kept builds nothing: asked $(< "${asked_file}")" >&2
  failed=1
fi
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\nsrc/old.rs 2')
sharp $'src/old.rs 1\nsrc/old.rs 2' merges "${sha}" \
  "lets src/old.rs rise from 1 to 2: ${sha:0:12}'s reader counts lines 1 there" \
  "a listed file rises from main's count by an expect only the head's reader counts"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${raise}")
sharp $'src/split.rs 1\nsrc/split.rs 1' refuses "${sha}" \
  "but only 1 of the lines its reader counts there" "a line named twice pays for one"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\nsrc/note.sh 1\nsrc/old.rs 1')
sharp "src/note.sh 2" refuses "${sha}" "and only a Rust file's may rise" \
  "a file other than Rust may not rise, whatever its comments say"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\nsrc/dir.rs 1\nsrc/old.rs 1')
sharp "src/dir.rs 1" refuses "${sha}" "and only a Rust file's may rise" \
  "a folder named as Rust is no Rust file"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\nsrc/link.rs 1\nsrc/old.rs 1')
sharp "src/link.rs 1" refuses "${sha}" "and only a Rust file's may rise" \
  "nor is a link named as Rust"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "src/gone.rs=fn gone() {}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\nsrc/gone.rs 1\nsrc/old.rs 1')
sharp "src/gone.rs 1" refuses "${sha}" "doesn't leave the file as main has it" \
  "a file main lacks may not rise"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\nsrc/old.rs 1\nsrc/\303\251.rs 1')
sharp $'src/\303\251.rs 1' merges "${sha}" $'lets src/\303\251.rs rise from 0 to 1' \
  "a path past ASCII is named as git names it, unquoted"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\n:a.rs 1\nsrc/old.rs 1')
sharp ":a.rs 1" merges "${sha}" "lets :a.rs rise from 0 to 1" \
  "and one whose name opens as a pathspec's magic would is read as written"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\n./src/old.rs 1\nsrc/old.rs 1')
sharp "./src/old.rs 2" refuses "${sha}" "naming it other than as git does" \
  "a file named other than by its path from the top may not rise"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\n:(top)src/old.rs 1\nsrc/old.rs 1')
sharp ":(top)src/old.rs 2" refuses "${sha}" "naming it other than as git does" \
  "nor one named through a pathspec's magic"
sha=$(sharpened "${raise}")
sharp "${split_named}" refuses "${sha}" "src/split.rs 2, main has 0" \
  "a rise with main's own reader is refused, as any rise was"
if [[ -e ${asked_file} ]]; then
  echo "FAIL a head keeping main's reader builds nothing: asked $(< "${asked_file}")" >&2
  failed=1
fi
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${raise}" \
  $'src/split.rs=#[cfg_attr(\n    test,\n    allow(dead_code)\n)]\nfn split() { 1; }')
sharp "${split_named}" refuses "${sha}" "doesn't leave the file as main has it" \
  "a rise in a file the head changes is refused, as the rise may be its own"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${raise}" "src/new.rs=#[allow(x)]")
sharp "${split_named}"$'\nsrc/new.rs 1' refuses "${sha}" "src/new.rs: 1, listed 0" \
  "main's reader still refuses a lint the head turns off beside a rise"
if [[ -e ${asked_file} ]]; then
  echo "FAIL a head main's reader refuses builds nothing: asked $(< "${asked_file}")" >&2
  failed=1
fi
printf '%s\n' "${split_named}" "src/new.rs 1" > "${named_file}"
said=$(PATH="${work}/fake-nix:${PATH}" bash "${gate}" "${sha}" 2>&1 || true)
if [[ ${said} != *"src/new.rs: 1, listed 0"* || ${said} == *"lets src/split.rs rise"* ]]; then
  echo "FAIL a rise main's reader then refuses isn't said as let through: ${said}" >&2
  failed=1
fi
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${raise}")
sharp "src/split.rs 1" refuses "${sha}" "but only 1 of the lines its reader counts there" \
  "a file rises no further than by the lines named"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\nsrc/old.rs 1\nsrc/split.rs 18446744073709551616')
sharp "src/split.rs 1" refuses "${sha}" "but only 1 of the lines its reader counts there" \
  "nor does one by a count too long for bash's sums"
# Two files rise at once, each judged: both said when both fit, and
# neither when either fails, whichever comes first.
two="nix/suppressions-waiting.txt=# Waiting."$'\nsrc/old.rs 1\nsrc/split.rs 2\nsrc/\303\251.rs 1'
both_named="${split_named}"$'\nsrc/\303\251.rs 1'
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${two}")
sharp "${both_named}" merges "${sha}" "lets src/split.rs rise from 0 to 2" \
  "two files rise, each by the lines named"
sharp "${both_named}" merges "${sha}" $'lets src/\303\251.rs rise from 0 to 1' \
  "and each is said"
# Whether the gate, judging `sha` with the head's reader naming `named`,
# said no rise as let through.
none_let() {
  printf '%s\n' "$1" > "${named_file}"
  said=$(PATH="${work}/fake-nix:${PATH}" bash "${gate}" "$2" 2>&1 || true)
  if [[ ${said} == *"lets "* ]]; then
    echo "FAIL a rise beside one refused isn't said as let through: ${said}" >&2
    failed=1
  fi
}
sharp "${split_named}" refuses "${sha}" $'raises src/\303\251.rs from 0 to 1, but only 0' \
  "one of two that rises unnamed is refused"
none_let "${split_named}" "${sha}"
sharp $'src/split.rs 1\nsrc/\303\251.rs 1' refuses "${sha}" \
  "raises src/split.rs from 0 to 2, but only 1" "whichever of the two it is"
none_let $'src/split.rs 1\nsrc/\303\251.rs 1' "${sha}"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${two}" \
  $'src/\303\251.rs=#[cfg_attr(test, allow(x))]\nfn changed() {}')
sharp "${both_named}" refuses "${sha}" "doesn't leave the file as main has it" \
  "a second risen file the head changes is refused"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${two}" \
  $'src/split.rs=#[cfg_attr(test, allow(x))]\nfn changed() {}')
sharp "${both_named}" refuses "${sha}" "doesn't leave the file as main has it" \
  "as is a first"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" \
  "nix/suppressions-waiting.txt=# Waiting."$'\nsrc/old.rs 2')
sharp "src/old.rs 2" refuses "${sha}" "but only 0 of the lines its reader counts there" \
  "a line main's reader counts already doesn't raise its file"
sha=$(sharpened "nix/counts.sh=${sharper_reader}" "${raise}")
sharp "src/split.rs one" refuses "${sha}" "as other than PATH LINE" \
  "named lines the gate can't read are refused"
NIX_FAILS=1 sharp "src/split.rs 1" refuses "${sha}" "error: the build failed" \
  "a build of the head's lines that fails stops the gate in nix's words"
NIX_FAILS=1 sharp "src/split.rs 1" refuses "${sha}" \
  "fix the head if its reader or flake fails, or land it from a machine that builds" \
  "and says what may be at fault"
if [[ -e ${ran_outside} ]]; then
  echo "FAIL the head's reader runs only in its build, never where the gate runs" >&2
  failed=1
fi

finish
