# Tests

Tests of the `mycelium` command as its own process, for what only shows
across one: what it reads from its environment, what it leaves on disk,
and how much memory it holds.
The rest of the tests sit beside the code they prove, in `src/`.

- `forages.rs`: the node's forage records made for ten thousand cells
  within 64 MB, what was read for them included, in time that grows
  with the cells; short of the 30 MB target while each record carries
  its quest's documents, about 5 KB a cell, made for each ask and never
  held, a gap that closes by noon on 18 November 2026, by writing the
  records as raw bytes as the cells' do
- `held.rs`: a holder told to end as its scope's stop tells it, with a
  window joined, ending as its program does
- `made.rs`: a machine made for a start from a provider's offer, linked,
  started on and let go at its time or once it hands over, each node and
  link a process of its own
- `murmurs.rs`: ten thousand cells' murmurs read within 30 MB, in time
  that grows with the cells, and read again after one more in a small
  share of it, the memory counted as `networks.rs` counts it
- `named.rs`: a node given `--machine` recording the name, and a command
  with none going by it rather than the system's
- `networks.rs` and `cells.rs`: the node's network and cell records each
  built for ten thousand cells within 30 MB and 64 MB, each cell holding
  two trees, in time that grows with the cells, the memory counted by an
  allocator of its own, which only a program of its own may hold
- `realm/`: the realm of ten thousand cells both are built for, and the
  bar they are held to
- `shared.rs`: a quest shared and waited for, printing one JSON line
  however the wait ends and failing unless the machine holds it
- `receptors.rs`: what `mycelium receptors say` leaves on disk, its
  owner's alone, and each word it refuses, so nothing a receptor read can
  reach the file
- `spellings.rs`: each old spelling of a command, and each silent alias
  such as `network` or `--json`, saying byte for byte what the tree
  that took it in says
- `ssh.rs`: what another machine's link key may run, as SSH runs it
- `stopped.rs`: a node told to stop while a client watches it, letting
  go, taking its socket away and ending well
