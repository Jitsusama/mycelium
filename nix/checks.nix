# Every check `nix flake check` runs: the crate built with its tests, held to
# clippy and rustfmt, and every other tracked file to its own linter, with the
# tree's own rules (tree.sh), with no real name in it; and on Linux, the
# command linked in whole.
{
  pkgs,
  package,
  # The command linked in whole, or null on a system with no musl.
  static,
  source,
}:
let
  # A check that runs `script` in a copy of the repository, with `tools` on
  # its path, and passes when the script does.
  inTree =
    name: tools: script:
    pkgs.runCommand name { nativeBuildInputs = tools; } ''
      cp -r ${source} tree
      chmod -R u+w tree
      cd tree
      ${script}
      touch $out
    '';
in
{
  # The crate builds and its tests pass.
  mycelium = package;

  # clippy finds nothing, tests included, at the lints Cargo.toml sets.
  mycelium-passes-clippy = package.overrideAttrs (old: {
    pname = "mycelium-passes-clippy";
    nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
    buildPhase = ''
      cargo clippy --all-targets --offline -- -D warnings
    '';
    doCheck = false;
    installPhase = ''
      touch $out
    '';
  });

  # Rust is laid out as rustfmt lays it out.
  rust-is-formatted = inTree "rust-is-formatted" [ pkgs.rustfmt ] ''
    rustfmt --edition 2024 --check src/*.rs src/*/*.rs
  '';

  # Nix is laid out by nixfmt, with nothing unused and nothing statix flags.
  nix-is-clean = inTree "nix-is-clean" [ pkgs.nixfmt pkgs.deadnix pkgs.statix ] ''
    nixfmt --check flake.nix nix/*.nix
    deadnix --fail flake.nix nix
    statix check .
  '';

  # TOML is laid out by taplo and reads as TOML.
  toml-is-clean = inTree "toml-is-clean" [ pkgs.taplo ] ''
    taplo fmt --check
    taplo lint
  '';

  # Markdown keeps markdownlint's rules, as .rumdl.toml sets them.
  markdown-is-clean = inTree "markdown-is-clean" [ pkgs.rumdl ] ''
    rumdl check .
  '';

  # The repository is public, so no file names a real machine, employer,
  # person's account or place: tests and docs say kestrel, skiff, osprey and
  # tern for machines, and the documentation ranges (192.0.2.x, 2001:db8::)
  # for addresses. It reads every tracked file but this one, which has to
  # name the words, and the lock file, which only names pins.
  names-stay-out =
    let
      real = [
        "optimus"
        "penelope"
        "methuselah"
        "equilibrius"
        "shopify"
        "grrbrr"
        "joel-gerber"
        "joel.gerber"
        "192.168.4."
      ];
    in
    inTree "names-stay-out" [ pkgs.ripgrep ] ''
      if rg --ignore-case --fixed-strings --line-number --no-heading --hidden \
        --glob '!nix/checks.nix' --glob '!flake.lock' \
        ${pkgs.lib.concatMapStringsSep " " (word: "-e ${pkgs.lib.escapeShellArg word}") real} .; then
        echo "The lines above name a real machine, employer, account or place."
        exit 1
      fi
    '';

  # Every folder says what it holds, and every file is read by a linter.
  tree-keeps-its-rules = inTree "tree-keeps-its-rules" [ pkgs.shellcheck ] ''
    shellcheck --enable=all nix/tree.sh
    bash nix/tree.sh
  '';
}
// pkgs.lib.optionalAttrs (static != null) {
  # The command linked in whole needs no library, so it runs where no Nix
  # is. Its tests pass as it builds, as the other build's do.
  mycelium-needs-no-libraries =
    pkgs.runCommand "mycelium-needs-no-libraries" { nativeBuildInputs = [ pkgs.file ]; }
      ''
        said=$(file --brief --dereference ${static}/bin/mycelium)
        case $said in
          *"static-pie linked"* | *"statically linked"*) ;;
          *) echo "the static mycelium is linked to libraries: $said"; exit 1 ;;
        esac
        touch $out
      '';
}
