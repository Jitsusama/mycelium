# Working on Mycelium

Mycelium is a nimble core for agent networks: one small node per machine,
linked with the nodes of the other machines in its realm. The
[README](README.md) says what it is for, and [PROTOCOL.md](PROTOCOL.md)
says what crosses between machines and how clients speak to a node.

## Map

- `src/`: the crate, one module per concern; `src/lib.rs` lists them
- `src/store/`: where a node keeps messages and presence
- `src/link/`: two machines' stores kept as one, with its seams for
  dialling, discovery and membership
- `benches/`: how fast the node is, each bench a program printing what
  it measured
- `tests/`: the `mycelium` command tested as its own process
- `nix/`: the package, the checks and the tree's own rules
- `scripts/`: the merge gate, the merge train and their cases

## Rules

- Each seam later growth plugs into is a trait with one implementation;
  a new kind of machine, connection or store is a new implementation,
  never a branch inside an old one. A seam with outside effects (the
  store) has a fake that keeps the same rules; the others are tested
  through their own implementation over an in-memory pipe.
- Words are core.nix's glossary's: machine, not host; session, peer,
  group and address as the mesh means them; node, link and realm as
  PROTOCOL.md says.
- The mesh's own folders are read and written as the mesh writes them,
  so pi sessions never notice the node.
- A lint is silenced only with `#[expect]` and its reason.
- A test waits on the change it needs, never on a fixed sleep.
- The repository is public, so nothing names a real machine, employer,
  account or place: machines are kestrel, osprey, skiff and tern, and
  addresses come from the documentation ranges. The `names-stay-out`
  check fails on the real ones.

## Commands

```sh
nix flake check                    # the gate: every check there is
nix-offload there . checks.aarch64-darwin.mycelium  # the gate on a Mac
nix develop -c cargo test          # the tests, as you go
nix develop -c cargo clippy --all-targets -- -D warnings
nix develop -c cargo fmt           # lay out the Rust
nix fmt -- flake.nix nix/*.nix     # lay out the Nix
```

## Bindings

How this repository keeps Joel's repository standard:

- **Gate:** `nix flake check`, run locally, and the Mac's build and
  tests, `checks.aarch64-darwin.mycelium`, built on a Mac through
  core.nix's `nix-offload there`, since what compiles on Linux can fail
  on macOS (its `mode_t` is 16 bits, not 32); there is no hosted CI.
  Where `nix-offload` can't reach a Mac, the same check is built with
  plain `nix build` on one whose Nix sets `sandbox = relaxed`, which
  lets this check alone out of the sandbox it asks to leave; a Mac with
  the sandbox fully on refuses it. Where no Mac answers at all, the
  merge says so in its Validation, and the Mac build runs before
  core.nix takes it.
  A merge goes only through `scripts/merge-gate.sh`, main's own copy,
  which refuses a head that doesn't hold main as it is now, or that
  lacks any Linux check kept as passed at that very commit. So rebase
  onto main and build the checks there just before merging. Where
  several reviewed changes are ready at once, they ride the merge train
  instead, named by their reviewed heads' full shas. Run main's own
  copy, `git show origin/main:scripts/merge-train.sh | bash -s -- make
  SHA...`, after `git fetch -q origin main`: it lays them onto main as
  one tip, the checks build there once, and `land TIP` moves main to
  it through main's gate, each change then reading merged.
- **Linters:** clippy with pedantic and rustc's own lints, as
  `Cargo.toml` sets them; rustfmt; nixfmt, deadnix and statix; taplo;
  rumdl, as `.rumdl.toml` sets it; shellcheck with every optional check.
  `nix/tree.sh` fails on any file no linter reads, and lists the files
  that need none.
- **Tests:** unit tests beside the code they prove, with the fakes in
  `src/store/memory.rs`; tests of the command as its own process go in
  `tests/`.
