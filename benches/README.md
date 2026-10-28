# Benches

How fast the node is, each bench a program of its own that prints what it
measured. They run on the machine at hand, so a figure is only as quiet as
that machine was; each prints its load beside it.

- `heard.rs`: how long a post takes from being written into the mesh's
  folders, as a pi session writes one, to being heard by a watch on the
  node's socket.

```sh
nix develop -c cargo bench --bench heard -- --watches 100 --rate 200 --seconds 10
```
