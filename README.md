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
- A quest carried with `mycelium carry --to` goes to another machine with
  its sessions: each carries its work into the quest's handover and ends
  here, and starts again there once its trees have arrived.
- A program started with `mycelium holds add` outlives the window it
  started in, `mycelium holds` lists what is held, and `mycelium holds
  <ID> attach` takes one up again from any terminal on the machine. The
  older `hold`, `held`, `attach <ID>` and `again` still work. One that
  exits 75 is parked: its window stays, and a key or its wake file
  starts it again. While one held there is at work (a turn, queued
  work, or a program that is no session), the node keeps it from
  sleeping when idle, until its battery drops below 20%. Shutting its
  lid or asking it to sleep always sleeps it.
- `mycelium attach --host` shows a conversation a pi host holds until
  the host lets it go. It exits 3 when the host let the conversation go
  back to plain pi, as a release does, so whatever runs in the window
  next can carry on as that pi, and 69 when the host stayed gone past
  its two minutes for a restart, so it can wait for the host to come
  back; any other leaving exits 0.
- Links run over SSH, so a machine needs nothing but the `mycelium`
  command and an SSH login.
- Each node serves a local socket in niri's shape, which pi, Quickshell
  or a terminal reads, and says on it what each machine is (`mycelium
  machines`, or `machines <NAME>` for one), the facts work will be
  placed by. A client that opens with
  `hello` is answered in the daemon interface every daemon on the machine
  speaks, and watches the realm's sessions, machines and messages
  change.
- `mycelium health` prints how the realm's network stands as one JSON
  line for a daily read: each link and how often it dropped in the last
  day, why a link was refused (such as a machine on another link version,
  saying which to update) while none reaches it, each machine's
  sessions, the presences still saying they run with their process gone,
  and how long the node took to answer.
- `mycelium swarm` shows the realm's teams in a terminal, nested as
  they are and those needing you first, with how many of each one's
  sessions need you, are stuck, move or wait; `mycelium swarm <TEAM>`
  shows one team's sessions and where it meets the others, and
  `--json` prints each team's record.
- `mycelium networks` draws the realm's networks as a tree under their
  shapes (orchestrations, projects, repositories, groups and the
  machines), with how many of each one's sessions need you, are stuck,
  move, wait or lie dormant; `--cells` puts each network's sessions
  under it, and `mycelium networks <NAME>` draws one with its cells.
  `mycelium cells` lists every session worst first, then the machines,
  and `mycelium cells <NAME>` says what one is and which networks it
  joined. Both print JSON lines with `-o json`, networks first, then
  cells, the model a page draws the mycelium from.
- `mycelium swarm say <NAME> <WORDS>` says something as the person to
  a team, which its lead hears, or to one session;
  `mycelium swarm pause|resume <NAME>` pauses or resumes every session
  of a team and the teams within it, or one session, and says back who
  was told.
- `mycelium web <QUEST> say|tie|settle` speaks in a quest's web, the
  murmurs and ties its sessions share: each machine appends to its own
  file in the quest's `web` scratch folder, which a share carries like
  any other, and `mycelium web <QUEST> fold --into <FILE>` folds every
  machine's into what the web says now, for a page to read.

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
mycelium --realm work holds add -- pi
mycelium --realm work holds
mycelium --realm work holds 5b992038 attach
```

## Lineage

An earlier experiment of the same name proved the concept on NATS. This
core keeps the concept and none of the code.
