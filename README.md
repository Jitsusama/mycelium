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
- A session on another machine is seen with the machine it runs on, and
  let go when it ends or the link drops.
- A quest shared with `mycelium share` keeps the same record on every
  linked machine, with a journal written on both keeping both entries.
- Links run over SSH, so a machine needs nothing but the `mycelium`
  command and an SSH login.
- Each node serves a local socket in niri's shape, which pi, Quickshell
  or a terminal reads.

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
```

## Lineage

An earlier experiment of the same name proved the concept on NATS. This
core keeps the concept and none of the code.
