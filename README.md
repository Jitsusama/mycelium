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
  starts it again. One that resumes a session waits its turn, so a
  restart's sessions come back four at a time, those waiting on their
  person first; a key typed in its window starts it now. While one
  held there is at work (a turn, queued
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
  placed by. Beside them it says the builders this machine sends Nix
  builds to, read from Nix machines files given as `--builders` and
  `--lent-builders`: what each builds and whether it answers, a lent one
  never dialled so as not to wake what costs to run. Given its machine's
  own health daemon's socket as `--health-socket`, it carries in its
  machine the words that daemon says of the whole machine and each
  part, so every machine reads how every other stands. A client that opens
  with `hello` is answered in the daemon interface every daemon on the machine
  speaks, and watches the realm's sessions, machines, messages, teams
  and networks change, each network made as `mycelium networks` makes
  it.
- `mycelium health` prints how the realm's network stands as one JSON
  line for a daily read: each link and how often it dropped in the last
  day, why a link was refused (such as a machine on another link version,
  saying which to update) while none reaches it, each machine's
  sessions, the presences still saying they run with their process gone,
  and how long the node took to answer.
- `mycelium balance` prints how evenly the realm's machines carry its
  work as one JSON line: each machine's load (the largest of the memory
  it uses, how busy its processors are and how long its work stalls),
  whether it is over or has room,
  its busy and idle sessions, how far the loads spread, and flags for a
  machine over while another has room, one resource spent while the
  other idles, and facts too old to place by. `--follow` reads each
  minute, says the flags seen in most of the last ten, and keeps each
  line in the state folder's `balance`, one file a day.
- `mycelium placements` judges in hindsight how well this node placed
  work over the last hour (`--hours` for more), from the placements it
  kept and the balance kept beside them, as one JSON line: each start
  whose machine, a minute on, carried more than another it could have
  gone to with room, by more than the dead band; each herd of three or
  more starts onto one machine within a minute while another had more
  headroom; how many cores each busy session used on each machine, in
  the middle and at the high; and flags for regrets past a tenth and for
  herds.
- `mycelium unanswered` prints what holds the mesh up as JSON lines,
  oldest first: each question of the last two hours still waiting on a
  peer it was put to, and each hold still standing. `mycelium said`
  prints what you said to sessions through this machine, with when each
  was taken up and answered.
- `mycelium swarm` shows the realm's teams in a terminal, nested as
  they are and those needing you first, with how many of each one's
  sessions need you, are stuck, move or wait; `mycelium swarm <TEAM>`
  shows one team's sessions and where it meets the others, and
  `--json` prints each team's record.
- `mycelium networks` draws the realm's networks as a tree under their
  shapes (orchestrations, projects, repositories, trees two cells or
  more hold at once, groups and the machines), with how many of each
  one's sessions need you, are stuck, move, wait or lie dormant;
  `--cells` puts each network's sessions under it, and
  `mycelium networks <NAME>` draws one with its cells.
  `mycelium cells` lists every session worst first, then the machines
  and the builders, and `mycelium cells <NAME>` says what one is, which
  networks it joined and the trees it holds alone. A long list folds,
  so a realm of thousands of sessions still fits a screen: the
  worrying first, then the busiest few, then one row
  counting the rest; `--all` unfolds it, and a name opens any one.
  `mycelium cells --standing needs-you,stuck` lists only the cells
  standing so. `-w` on either draws it again in place every two
  seconds, as it changes, until stopped. Both print JSON lines with
  `-o json`, networks first,
  then cells, then what the cells did, the model a page draws the
  mycelium from: a session's cell also says its id, what it asks, its
  repository, the folder it works in as `cwd`, its plan's next line and
  last choice as `plan`, its usage today and the orchestrations it
  leads, and each `event` line says a cell's commit, push, change
  proposed or merged, or stage moved, newest first. `mycelium networks -o json --counts`
  prints the networks alone, a summary for an agent to read.
- A receptor senses one outside source for its person, such as Slack,
  intake only, and says how it stands with `mycelium receptors say`:
  counts and times alone, never what it read. The node judges each,
  marking one whose process is gone `off` and one fallen late
  `failing`, and the networks draw it as a cell of its source's
  `outside` network, beyond the machines; `mycelium cells` lists each
  with what it brought in today.
- `mycelium forage` says what each cell is for: its purpose, the first
  sentence of its quest's Summary, and each of its plans, research and
  reports with its stage and how many steps are ticked. Named by its
  address or its quest's id, it says one cell's; `-o json` prints a
  JSON line each. Each node serves its own cells' forages as the
  `forage` kind, read from its quest store as it changes.
- `mycelium swarm say <NAME> <WORDS>` says something as the person to
  a team, which its lead hears, or to one session;
  `mycelium swarm pause|resume <NAME>` pauses or resumes every session
  of a team and the teams within it, or one session, and says back who
  was told.
- `mycelium web <QUEST> say|tie|settle` speaks in a quest's web, the
  murmurs and ties its sessions share: each machine appends to its own
  file in the quest's `web` scratch folder, which a share carries like
  any other, and `mycelium web <QUEST> fold --into <FILE>` folds every
  machine's into what the web says now, for a page to read. The node
  serves each murmur this machine's cells said in the last day as a
  `murmur` record, newest first, for a page to watch.

[PROTOCOL.md](PROTOCOL.md) says how, and what comes next: shared scratch,
the node's own log, connections of its own and providers for machines
such as hosted sandboxes.

## Running It

```sh
# On each machine the node reaches, only the command is needed.
# On the machine that dials:
mycelium --realm work node --ssh jsama@192.0.2.32

# Then, on either machine, share a quest with every machine it links:
mycelium --realm work share QEST-20261013-4UHC5Q

# Hold a session, close its window, and take it up again elsewhere:
mycelium --realm work holds add -- pi
mycelium --realm work holds
mycelium --realm work holds 5b992038 attach
```

Bare `mycelium` lists every command, and each command's `--help` says
what it does in a line and gives an example to type.

## Lineage

An earlier experiment of the same name proved the concept on NATS. This
core keeps the concept and none of the code.
