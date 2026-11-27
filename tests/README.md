# Tests

Tests of the `mycelium` command as its own process, for what only shows
across one: what it reads from its environment, what it leaves on disk,
and how much memory it holds.
The rest of the tests sit beside the code they prove, in `src/`.

- `declared.rs`: a node given `--declared` saying its machine as that
  file declares, never as the account's own declaration says
- `forages.rs`: the node's forage records made for ten thousand cells
  within 64 MB, what was read for them included, in time that grows
  with the cells; short of the 30 MB target while each record carries
  its quest's documents, about 5 KB a cell, made for each ask and never
  held, a gap that closes by noon on 18 November 2026, by writing the
  records as raw bytes as the cells' do
- `gone.rs`: a holder of a window on a pi host's conversation keeping
  the window when the host stops before answering its join, trying again
  at a key while nobody takes a join at its socket and while the socket
  is gone, and joining it once it's back
- `held.rs`: a holder told to end as its scope's stop tells it, with a
  window joined, ending as its program does
- `made.rs`: a machine made for a start from a provider's offer, linked,
  started on and let go at its time or once it hands over, each node and
  link a process of its own; nothing of the asker's environment, or its
  node's, reaching the work there, as nothing of a carried session's does
- `murmurs.rs`: ten thousand cells' murmurs read within 30 MB, in time
  that grows with the cells, and read again after one more in a small
  share of it, the memory counted as `networks.rs` counts it
- `named.rs`: a node given `--machine` recording the name, and a command
  with none going by it rather than the system's
- `networks.rs` and `cells.rs`: the node's network and cell records each
  built for ten thousand cells within 30 MB, each cell holding two
  trees, in time that grows with the cells, the memory counted by
  an allocator of its own, which only a program of its own may hold
- `piped.rs`: a machine's page drawn into a pipe against a node of its
  own, saying when it was measured with its day, date and zone
- `process/`: the `mycelium` command with its folders under a test's own
  home, and the wait for a path it makes, that `declared.rs`,
  `named.rs`, `piped.rs`, `stopped.rs` and `unanswered.rs` share
- `realm/`: the realm of ten thousand cells both are built for, and the
  bar they are held to, with the counting allocator and thread time
  (`counting.rs`) that `ties.rs`, `forages.rs` and `murmurs.rs` share
- `rejoined.rs`: a window's join of a session on another machine, run
  again each time SSH drops after saying it reconnects, ending as the
  join there ends, once the node says the session has, or on Ctrl+C
- `ties.rs`: the node's tie records made for a hundred, a thousand and
  ten thousand cells, five power-law ties a cell, within 30 MB, the
  memory a cell growing within a ceiling of three times from a
  thousand and the time within five, against a target of 1.2; and the
  node's lock held within 5 ms to take the 50,000 messages it keeps
  for the fold
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
- `unanswered.rs`: what waits on the mesh into a pipe, text for a person
  with nothing cut, and as JSON lines with `-o json` or `--json`
- `unread.rs`: a command's failure said to a pipe nobody reads any more
  ending as a failure, not a panic
