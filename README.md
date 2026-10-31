# Mycelium

A nimble core for agent networks. One small node runs on each machine,
whether it is the one Joel sits at, one on his network or one in a cloud,
and the nodes of a realm make one network of agents that talk and share in
groups. A node with no links is the whole network, so nothing here needs a
cloud.

## What It Does Today

The harness mesh's messages and presence, carried between machines:

- A message any pi session sends reaches the sessions of every linked
  machine, and theirs reach it.
- A session on another machine is seen with the machine it runs on, let
  go when it ends, and shown as unreachable while no link reaches its
  machine.
- A quest shared with `mycelium share` keeps the same record on every
  linked machine, with a journal written on both keeping both entries,
  and the same folders of its scratch, those named with `--scratch`.
- A program started with `mycelium hold` outlives the window it started
  in, and `mycelium attach` takes it up again from any terminal on the
  machine. While one held there is at work (a turn, queued work, or a
  program that is no session), the node keeps it from sleeping when
  idle, and on Linux with its lid shut too, until its battery drops
  below 20%.
- Links run over SSH, so a machine needs nothing but the `mycelium`
  command and an SSH login.
- Each node serves a local socket in niri's shape, which pi, Quickshell
  or a terminal reads, and says on it what its machine is (`mycelium
  machine`), the facts work will be placed by. A client that opens with
  `hello` is answered in the daemon interface every daemon on the machine
  speaks, and watches the realm's sessions and machines change.

[PROTOCOL.md](PROTOCOL.md) says how, and what comes next: shared scratch,
the node's own log, connections of its own and providers for machines
such as hosted sandboxes.

## Running It

```sh
# On each machine the node reaches, only the command is needed.
# On the machine that dials:
mycelium --realm work node --ssh joel.gerber@192.168.4.32

# Then, on either machine, share a quest with every machine it links:
mycelium --realm work share QEST-20261013-4UHC5Q

# Hold a session, close its window, and take it up again elsewhere:
mycelium --realm work hold -- pi
mycelium --realm work held
mycelium --realm work attach 5b992038
```

## Lineage

An earlier experiment of the same name proved the concept on NATS. This
core keeps the concept and none of the code.
