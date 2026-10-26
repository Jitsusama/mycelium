#!/usr/bin/env bash
# The tree's own rules, run from the repository's root: every folder says
# what it holds, and every file is one a linter reads or one listed here as
# needing none, with the reason.
set -euo pipefail

# The files git keeps or would keep, so what it ignores (builds, caches) is
# never judged; in a check's copy of the tree, with no git, every file.
if git rev-parse --git-dir >/dev/null 2>&1; then
  listed="$(git ls-files --cached --others --exclude-standard)"
else
  listed="$(find . -type f -printf '%P\n')"
fi
sorted="$(sort <<<"${listed}")"
mapfile -t files <<<"${sorted}"

failed=0

# A folder says what it holds in a README, or in the opening comment of its
# entry file when it holds one kind of file. Every folder above a file is
# one, so a folder holding only folders is judged too.
ancestors="$(awk -F/ '{ path = $1; for (i = 1; i < NF; i++) { print path; path = path "/" $(i + 1) } }' <<<"${sorted}")"
unique="$(sort -u <<<"${ancestors}")"
mapfile -t dirs <<<"${unique}"
for dir in "${dirs[@]}"; do
  [[ -n "${dir}" ]] || continue
  if [[ ! -e "${dir}/README.md" && ! -e "${dir}/mod.rs" && ! -e "${dir}/lib.rs" ]]; then
    echo "${dir} says nowhere what it holds: give it a README.md" >&2
    failed=1
  fi
done

# Each kind of file and what reads it; the files that need no linter are
# listed by name with the reason.
for file in "${files[@]}"; do
  case "${file}" in
    *.rs) ;;         # rustc, clippy and rustfmt
    *.nix) ;;        # nixfmt, deadnix and statix
    *.toml) ;;       # taplo
    *.md) ;;         # rumdl
    *.sh) ;;         # checked by shellcheck
    Cargo.lock) ;;   # written by cargo
    flake.lock) ;;   # written by nix
    LICENSE) ;;      # the licence's own text
    .gitignore) ;;   # a list of paths, read by git
    *)
      echo "${file} is read by no linter: add one, or list it here with the reason" >&2
      failed=1
      ;;
  esac
done

exit "${failed}"
