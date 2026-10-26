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
- `nix/`: the package, the checks and the tree's own rules

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

## Commands

```sh
nix flake check                    # the gate: every check there is
nix develop -c cargo test          # the tests, as you go
nix develop -c cargo clippy --all-targets -- -D warnings
nix develop -c cargo fmt           # lay out the Rust
nix fmt -- flake.nix nix/*.nix     # lay out the Nix
```

## Bindings

How this repository keeps Joel's repository standard:

- **Gate:** `nix flake check`, run locally; there is no hosted CI.
- **Linters:** clippy with pedantic and rustc's own lints, as
  `Cargo.toml` sets them; rustfmt; nixfmt, deadnix and statix; taplo;
  rumdl, as `.rumdl.toml` sets it; shellcheck with every optional check.
  `nix/tree.sh` fails on any file no linter reads, and lists the files
  that need none.
- **Tests:** unit tests beside the code they prove, with the fakes in
  `src/store/memory.rs`; tests of the node across processes go in
  `tests/` when there are some.
