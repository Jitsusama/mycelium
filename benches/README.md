# Benches

How fast the node is, each bench a program of its own that prints what it
measured. They run on the machine at hand, so a figure is only as quiet as
that machine was; each prints its load beside it.

- `heard.rs`: how long a post takes from being written into the mesh's
  folders, as a pi session writes one, to being heard by a watch on the
  node's socket. `--across` writes the posts on a second machine's
  folders, linked with the node's over a pipe, to time a link's own part.

```sh
nix develop -c cargo bench --bench heard -- --watches 100 --rate 200 --seconds 10
```

- `many.rs`: how `networks` and `cells` hold up at thousands of cells:
  each view's wall time and peak memory, the whole command run against
  a stand-in node serving a made-up mycelium, sized as live records are.

```sh
nix develop -c cargo bench --bench many -- --cells 1000,3000,10000
```

- `unanswered.rs`: how the node's view of what waits on the mesh holds
  up at thousands of cells: how long each of two hours' messages takes
  to judge as it comes, the slowest one, and how long the `unanswered`
  records take to make from it, with the memory it held.

```sh
nix develop -c cargo bench --bench unanswered -- --cells 1000,10000
```

- `watched.rs`: what a watch of sessions sends when a launcher opens
  after a while shut, at thousands of cells: the bytes of the changes it
  held back and how long finding them takes for one watch and for many,
  said whole and asked deep on each session's presence; then a list of
  a few fields of every session, each cut down from its bytes.

```sh
nix develop -c cargo bench --bench watched -- --cells 1000,10000 --watchers 1,50
```
