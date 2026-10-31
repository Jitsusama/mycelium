# Protocol

What mycelium keeps, what crosses between machines, and how a client
speaks to a node. Each seam below is a trait in the code, with one
implementation today and room for the next.

## Words

- **Machine:** one computer, whether Joel's, one on his network or one in
  a cloud. Every machine runs the same node.
- **Node:** mycelium's program, one per machine and realm.
- **Realm:** a set of machines that link, such as work or personal.
  Nothing crosses between realms, not even presence.
- **Link:** two machines' nodes joined, carrying what each holds to the
  other.
- **Session, peer, group, address:** as the harness mesh means them. A
  session is a session wherever it runs; one on another machine is a
  session whose machine is elsewhere.

## What Is Kept

Every record has one of these shapes, and its shape says how it crosses
between machines:

| Shape | Records | How it crosses |
| --- | --- | --- |
| Written once by id | messages | each machine holds the union of all |
| One writer | a session's presence, what a machine is | only its own machine writes it; others hold copies |
| Shared files | a shared quest's record | each machine holds the same, settled as below |
| This machine's own | read markers, the queue, what is said to `#machine`, held sessions | never |

Since nothing has two writers, no machine waits on another to agree, and
a machine asleep only means its records arrive when it wakes.

A message to the mesh's machine-wide group (`#machine`, or its older names
`#all` and `#global`) stays on its machine, and so does an answer to one:
a hold before optimus reboots means nothing to the Mac's sessions.

A machine has one mesh, so it belongs to one realm, recorded in
`~/.local/state/mycelium/realm` the first time a node or link runs there;
another realm is refused, so nothing of one realm reaches another.

## The Store

`store::Store` is where a node keeps messages and presence on its machine.
Today that is the harness mesh's own folders (`store::folders`), so pi
sessions go on reading and writing them unchanged while the node carries
them between machines:

- `messages/<id>.json`, one per message, the mesh's own shape.
- `presence/<session>.json`, one per session.

Every file is written whole, through a dot-file renamed into place, as the
mesh writes its own.

A session's own machine judges whether it lives, from its process, and is
told the moment it ends: on Linux the node holds each session's process
by a pidfd, which wakes it as the process exits, so nothing is judged on a
timer; a Mac judges every session again each few seconds until it has a
way of its own. A process that has exited but not been reaped has ended.
A process that holds a `Present` connection to its node (see The Socket)
is told ended the moment that connection closes, on either system, since
the kernel closes it however the process ends; without one, or between
one closing and the next, it is judged by its process as before.
The presence of a session elsewhere is kept with its process taken out, which
the mesh takes as there, and with `machine` naming where it runs. It never
goes over a session of this machine's or of a third machine's.

One link at a time carries a machine's sessions into a store: it claims
that machine first, by a lock in `~/.local/state/mycelium/claims/`, and a
second link to the same machine is refused until the first ends. The link
lets go of what it carried when the session ends. When the link drops,
it keeps what it carried but marks each `unreachableSince` the drop, in
milliseconds since the epoch, since a dropped link says nothing of
whether those sessions still run; what a link killed outright left is
marked by the node as it starts and stops, since that machine was last
linked. The next link to that machine settles them: the far end's `Have`
names its living sessions, those are carried again and unmarked, and the
rest are let go. While a link stands it touches each file it carried
once a minute, and a mark keeps the file's time, so a copy whose time
has stopped moving is one no link still vouches for.

A presence is carried only between the two machines it links, so every
pair of machines that should see each other's sessions links directly;
messages, being a union, reach every machine linked by any path.

## A Link

`link::run` runs one end of a link over any byte stream. Both ends run the
same code, each line a JSON value:

1. `{"Hello": {"machine", "realm", "version", "facts", "starts",
   "steers", "releases"}}`, first, from each end. Each end's membership
   admits the other or refuses it, and nothing else crosses until it
   has. `facts` says the end hears and says what a machine is; an end
   older than it says nothing of machines and is sent nothing. `starts`
   says it carries starts, and `steers` steers; an end older than either
   is asked none of it. `releases` says the end's way to the other
   end's machine can release that machine, as a provider's program can;
   only such an end is told the machine has handed over.
2. `{"Have": {"messages": [ids], "living": [sessions]}}`: every message
   the end holds within the link's reach, and its machine's living
   sessions. An end older than `living` names none, and every session of
   its machine out of reach is let go, to come again as it sends each.
   The reach goes back to when the two machines were last linked, and
   never past the mesh's week, so a link never hands back what
   the other end has let go. A first link reaches back to nothing, since a
   session takes every message that arrives as new, and a week of talk
   from before the machines were linked was never its to hear.
3. `{"Message": {"id", "body"}}`: a message the other end lacks, first
   for each the Have didn't list, then each new one as it lands.
4. `{"Presence": {"session", "body", "leases", "held"}}`: a living
   session of the sending end's machine, again whenever it, its leases
   or its `held` id change. `held` is the id a holder there keeps it by,
   which the other end writes into the carried presence as `heldAs`.
   `leases` are the mesh's `change` leases it and its hands hold on a
   repository's files, named as `core.nix:flake.nix`, so a session in
   another clone there is told it overlaps; one named by an absolute
   path names the sender's disk and stays, as does its `checkout`. The
   other end keeps them as one holding of the session's, marked with
   `machine`, never over a holding of its own machine's, and lets it go
   with the presence. An end older than `leases` sends none.
5. `{"Ended": {"session"}}`: a session of the sending end's machine has
   ended.
6. `{"Share": say}`: talk about shared quests, as below. An end that
   shares nothing sends none, so it still links with an end older than
   sharing.
7. `{"Machine": machine}`: what the sending end's machine is, measured
   as it is sent, as a node answers `"Machine"`: after the Have, before
   its sessions, and again each minute. The other end keeps it only if
   it names the sending machine, as ready while the link stands and as
   unreachable, with what it last said, once the link drops. Between
   two ends that both say `facts`, an end that has heard nothing at all
   for three minutes by the wall clock ends the link, so one left from
   before its machine slept gives way to its dialler's new one.
8. `{"Start": {"ask", "start"}}`: start a session held on the other
   end's machine, as a `"Start"` request says it. The other end hands it
   to its own node's socket, with a need for its own machine added, so
   that node admits or refuses it on its own facts as they are then.
   The end doesn't start it itself, since a process SSH started sits
   outside its person's login session.
9. `{"Started": {"ask", "reply"}}`: the other end's reply to the ask
   of that number: its node's, as its socket answered, for a start, and
   its own for a steer.
10. `{"Steer": {"ask", "address", "steer"}}`: put what the person says
    into the inbox of the session going by `address` on the other end's
    machine, as a `"Steer"` request says it. The other end writes it
    there itself, since a steer is only a file. A steer with an `id`
    opens a talk, which the other end follows from then on.
11. `{"Follow": {"talks": [{"address", "id", "has"}]}}`: after the
    Have, the talks the sending end follows that no stage has ended and
    that were written within a day, with how many bytes of each it has,
    so the other end carries on with those whose files are on its
    machine, across a dropped link.
12. `{"Replies": {"address", "id", "from", "lines"}}`: whole lines a
    session on the sending end's machine added to a talk's file, from
    byte `from` on, looked for each second. The other end adds only what
    its copy lacks, and only to a talk its own machine opened. A
    `replied` or `failed` stage ends the following, as does a day with
    nothing written.
13. `"HandedOver"`: the sending end's machine has handed over, sent at
    each minute's Machine to an end that `releases`, once that machine
    declares `leaves_at`, no work runs there, and some ran there since
    the link came or its handover has begun. Work is a session living
    there or a program held there, such as a build. The sender first says all
    it holds of the quests it shares. Frames cross in order, so if the
    other end then waits on no file from it, it holds all of them: its
    link ends, and its node releases the machine and dials it no more.
    An end older than this never hands over, so it is never released.

Three seams shape a link:

- **Dialling** (`link::dial::Dial`): how a machine reaches another's end,
  and how a terminal here runs a command there. SSH (`--ssh`): the
  other end is `mycelium link` run there, over the SSH session's pipes,
  and SSH has proved which machine it is. A machine's link key may be
  pinned in the other's `authorized_keys` to `mycelium --realm REALM
  ssh --from NAME`, which reads what was asked from
  `SSH_ORIGINAL_COMMAND` and runs only `mycelium link --realm REALM` or
  `mycelium --realm REALM attach ID`, each in plain words. That link
  admits only NAME, since the key proved it, and refuses an end saying
  it is another; anything else asked is refused, said to the asker, and
  kept as a JSON line in `refused-keys.jsonl` in mycelium's folder. Or a
  provider's program
  (`--via PROGRAM:PLACE`), for a machine SSH can't reach: run as
  `PROGRAM link --realm REALM PLACE`, it carries the link over its own
  input and output to `mycelium link` on that machine, and run as
  `PROGRAM run PLACE -- WORDS` from a terminal, it runs `WORDS` there,
  and run as `PROGRAM release PLACE` once that machine has handed over,
  it lets the machine go, as its provider destroys a machine it lent;
  a release that fails leaves the machine dialled again, so the
  program keeps its own pace. An end over SSH has 30 seconds to say
  hello, but a program's is waited on as long as the program runs,
  since it may first make the machine and put the far end there, and
  it ends when it can't. Its error output is passed on, and a line
  saying the other end "turned this machine away" is taken as a refused
  SSH key is. Either way, the far `mycelium link` looks each minute at
  the `mycelium` its search path names, and once that is another build
  than the one it runs, as after its machine's configuration changes,
  it ends, so its dialler starts the new one. A
  refused link isn't dialled again until its record in the node's
  `refused/` folder is removed, which the node's log names, so no
  retry of ours shows the other machine a failed login.
- **Discovery** (`link::discovery::Discovery`): which machines to link
  with, as they are found. Today, a list given to the node.
- **Membership** (`link::membership::Membership`): which machines to admit.
  Today, a realm: any machine of the same realm speaking the same version,
  never the machine itself.

## A Shared Quest

`mycelium share QEST-...` shares a quest from this machine, and each link
carries it from its next look. A quest is shared while its ledger,
`~/.local/state/mycelium/shared/<QUEST>.json`, says so; the ledger keeps
each file's version, counted per machine, and its content's hash.

The quest's record crosses: its README, journey, log, documents and
attachments, never a lock, a staging file or a copy kept beside. Of its
scratch, only the folders named cross, as `mycelium share QEST-...
--scratch notes` names `notes`, since a scratch can hold gigabytes of
clones and data. A folder holding more than 10,000 files or 1 GiB, or a
file over 64 MiB, is refused when named, and so are `tmp/` and build
output. Within a named folder, hidden files, links, `target/` and
`node_modules/` stay home. A folder once named stays shared.

A git clone, named or within a named folder, crosses as its `origin`
and branch, never file by file, since two machines writing one `.git`
break it. The other end clones any it lacks with `git clone`, beside
its link rather than in it, and never into a folder already there; one
that fails is told in the node's log and tried again at the next link.
Only a remote over the network crosses (`https://`, `ssh://` or
`user@machine:path`): a local path names a disk the other end lacks, and
a transport such as `ext::` runs a command. A clone with no such
`origin` is refused when named and stays home within a named folder.

Once a second, each end looks at its shared quests and says what changed,
as `Share` frames. A look reads over each record, a few dozen files, but
walks a named scratch folder only when a watch heard it change, when it
is newly named, or when it can't be watched; a watch that fails or
overflows has every folder walked. A change found while hearing the
other end is said at the next look:

- `{"Shares": {"quests", "scratch", "clones", "ended"}}`: every quest it
  shares, the scratch folders named of each, by quest, the clones in
  them, each `{"remote", "branch"}` by its path, and the quests whose
  README says `concluded` or `retired`. The other end starts sharing
  any quest or folder it didn't, first moving its own copy aside to
  `~/.local/state/mycelium/before-share/<when>/<QUEST>/`. It is said
  before the files, so a folder's files are carried once heard of.
  An ended quest's record still crosses, so the README saying it ended
  reaches the other end, but its scratch is let be. Once a quest has
  ended at an end and the other says it ended there too, or no longer
  names it, that end stops sharing it and isn't taken up again, its
  files left as they are. An end older than `ended` names none, so a
  quest stays shared with it.
- `{"Holds": {"quest", "files"}}`: each changed file's state.
- `{"Want": {"quest", "file", "at"}}`: a file whose state is newer
  there, from offset `at`. An end older than `at` names none and is
  sent the file whole.
- `{"Piece": {"quest", "file", "state", "at", "len", "body", "mode"}}`:
  for a file over 1 MiB, the 1 MiB of it at `at`, of `len` in all.
  The receiving end stages it under
  `~/.local/state/mycelium/pieces/<machine>/` and asks for the next, so
  the link's messages pass between pieces. Once whole, it is taken as a
  `File`; a piece that doesn't follow what is staged is passed over, and
  a link's start clears what an earlier one left.
- `{"File": {"quest", "file", "state", "body", "mode"}}`: the file, in
  base64, or no body once deleted, and its mode's permission bits, so a
  secret at 0600 lands at 0600. Its hash is checked before it is written,
  and it is made at its mode before anything is written to it. A file
  with no mode, as from an end older than `mode` or a merged journey,
  keeps its own. A change of mode alone waits for the file's next write.

A file changed on one end is taken by the other. One changed on both is
settled by the machine whose name sorts first, so the two never chase
each other: the journey and the log keep both ends' entries, and any
other file keeps the far end's beside it as `<file>.from-<machine>`, and
the settling node tells the quest's sessions so: a `finding` from
`mycelium@<machine>` to the quest's whole group, such as
`#quest/wcjqsg/4uhc5q`, named from the README's parents outermost first.
A deletion gives way to a write. A scratch file is taken as deleted only
while its named folder is there: a folder gone whole, as concluding a
quest takes a machine's whole scratch, is no word on another machine's
copy. Record text crosses with the home folder
marked, so a path under `/home/jitsusama` reads `/Users/joel.gerber` on a
Mac. Each write takes the quest's lock as the harness does and lands by
rename, so a session never reads half a file.

## The Socket

Each node serves one unix socket, `<runtime>/mycelium/<realm>.sock`,
reachable only by its account. The runtime folder is `$XDG_RUNTIME_DIR` on
Linux, and on macOS, which has none, the account's own temporary folder as
`getconf DARWIN_USER_TEMP_DIR` names it, not `TMPDIR`, which a session's
quest may move. The harness's machine queue and the disk guard keep their
records there too. It speaks as niri's does: a client writes one request as a
JSON line and reads one reply, `{"Ok": ...}` or `{"Err": "why"}`.

| Request | Reply |
| --- | --- |
| `"Sessions"` | `{"Sessions": [session]}`, this machine's and others' |
| `"Machine"` | `{"Machine": machine}`, this machine as measured now |
| `"Machines"` | `{"Machines": [machine]}`, this machine first, then each other |
| `{"Place": work}` | `{"Placement": placement}`, where the work goes |
| `{"Start": {"work", "folder", "command"}}` | `{"Started": {"machine", "held", "folder", "attach"}}` |
| `{"Steer": {"address", "steer"}}` | `{"Steered": {"machine"}}`, where it was put |
| `{"Messages": {"groups", "limit"}}` | `{"Messages": [message]}`, oldest first |
| `{"Send": {"from", "groups", "intent", "text", "re"}}` | `{"Sent": message}` |
| `{"Watch": {"groups", "recent"}}` | `"Watching"`, then events; no `groups`, no messages |
| `"Present"` | `"Present"`, then held open while its process runs |

A `Present` connection counts the process at its other end, as the
kernel names it, present until the connection closes; then every session
that process owns is ended at once. It says nothing more, and a later
`Present` from the same process takes its place, so the earlier one's
close ends nothing. A process no longer there, or one that is present
again, is let go of. A node that can't tell which process connected
refuses.

A session's `state` is `busy`, `idle`, `ended`, or `unreachable` while no
link reaches its machine. Beside its `id`, `address`, `machine` and
`groups`, it carries what its presence says of it: its `quest` by id (or,
from an older presence, the quest among what it owns), what it `wants` of
its person (`kind`, `since`, `said`) while it waits, and the `window` it
runs in (`terminal`, `id`, `within`, such as a kitty window's id and
kitty's socket). Only a session of this machine's has a `window`, since a
window is gone to on its own machine. A session a holder keeps, as one
started with `mycelium hold -- pi` is, says `held`: the id `mycelium
attach` takes it up by on its machine. It is known by the holder's
program being the session's own process, which only its own machine
sees, so a link carries it with the presence. A held session of this
machine's has as its `window` the latest window joined to its holder
that is still joined, or none. A held session says `attach` too, the
words that join it from a terminal on this machine, to be run as given:
`mycelium` here, or the dial's words for a machine a link of this node's
reaches, and none while no link does. Its
`presence` goes along whole, as its session wrote it, for a reader that
shows more of it, such as the launcher; a session elsewhere's has its
process taken out. Whether a session lives is its `state`, never
anything in its presence.

A machine is what its node measures of it when asked, the facts work is
placed by: its `name`, `measured_at`, `state` (`ready` for a node answering
for itself or a machine a link carries, `unreachable` for one no link
reaches now, as of when one last did), `os`, `system` (`linux` or
`macos`, `null` from a node too old to say it), `arch`, `model`, `cpu`
(`model`, `cores`), `memory` and `disk` in bytes (the disk holding its
account's home), `disk_band`, how full that disk is as the machine's
disk guard bands it (`healthy`, `pressure`, `urgent` or `critical`,
from the guard's `status.json` while its heartbeat is under two minutes
old, and absent otherwise), `accelerators` (each a `kind`, `gpu` or
`npu`, and its `driver`), and `power`: `on_battery`, true only with a
battery of its own and nothing plugged in, and `charge`, its own
batteries' mean in percent, `null` for a machine with none. A fact that
can't be read is `null`, never a guess.

Its `room` is the room it has for more work, as the harness's machine
queue says it, since that queue is what starts work there: `said_at`,
`memory_available` in bytes, `pressure` (`cpu`, `memory` and `io`, the
share of the last ten seconds some work waited on each, in percent),
`takes` (how many more `builds` and `vm_tests` it would start now, which
says nothing of a session) and `over`, the queue's limits the machine is
at or past now, as the queue judges them: `memory`, when it starts no
work, and `cpu`, when it starts no build beside a VM test. `over` is
empty when the machine is past none, so `takes` of none with nothing
`over` means a full queue, not a pressed machine, and `null` from a
queue too old to say. The room is `null` where no harness keeps a queue,
or where the queue's record is of another shape or unwritten for two
minutes. A machine keeps no reserve of mycelium's: its own slices keep
its person's share.

Its `declared` is what its configuration says of it, which can't be
measured: `reach`, the networks work there always reaches, such as
`internet`; `probes`, the networks it reaches only some of the time,
such as a laptop's `home`, each by a probe: `{"connect": "host:port"}`,
an address a connection opens to only while that network is reached, or
`{"run": [program, words]}`, a command run with no shell that exits 0
only then, for a network no address tells apart, such as one known by
its Wi-Fi or a tunnel's link; `outlives`, what a session held there outlives
(`window_close`, `node_restart`, `sleep`, `reboot`); `costs_money`;
`taints`, what it refuses, each a `key` work tolerates it by, an
`effect` (`never`, or `prefer_not` while anywhere else will do) and
`why`; and, where given, `cores`, how many processors work there may use
when the machine lists more than it is given, as a virtual machine held
to a share of its host does, which its `cpu`'s `cores` then says; and
`agents`, by realm, the command an agent session starts with there, such
as `{"personal": "pi-claude"}`, of which a realm hears only its own; and
`leaves_at`, the time it leaves the realm, as a machine lent for a while
does, in RFC 3339, after which it is no longer one of `"Machines"`; from
ten minutes before then, its node puts a steer into the inbox of each
session running there, once, saying when it leaves and to commit and
push, to put what can't be pushed and a note of what's next where its
work is shared, and to stop, with `from` `"machine"` and no `act`.
Where given, `within` names the machine it runs within, as a virtual
machine does its host, and a file the two share, `{"machine": "...",
"file": "..."}`; its `power` and `room` are then its host's, as the host
last said them there, while that is under two minutes old, since it
reads neither of its own truly. A host declares each machine within it
in `hosts`, in the same shape, and its node writes its `power` and
`room` to each file each minute, whole. Only
the machine's own built configuration writes it, as
`~/.config/mycelium/declared.json` (or under `XDG_CONFIG_HOME`), and
the machine enforces its own taints. It is `null` where nothing is
declared, or where the file isn't in this shape, which the node logs.

Its `reached` says whether each probed network answered, by name. The
node tries each probe every 30 seconds, giving each 3 to answer (a
command still running then is killed), and writes the round to
`reached.json` beside its sockets, where each end of a link on the
machine reads it; a record older than 90 seconds says nothing, so a
network is unknown rather than stale while no node probes.

`"Machines"` gives each other machine as its own end of a link last
said it, about once a minute.

`"Place"` asks which of those machines work should go to. The work says
its `needs` and `prefers`, each one of `{"machine": name}`,
`{"system": "linux"}`, `{"arch": "x86_64"}`, `{"reach": network}` or
`{"accelerator": "gpu"}`, the earliest preference most wanted (a
machine reaches a network it declares in `reach`, or one it probes
while its probe last answered, and lacks one whose probe didn't); the
keys of the taints it `tolerates`; whether it `may_wait` for a machine
out of reach; and how long it `lasts`, as an ISO 8601 duration such as
`PT2H` (`2h` reads too). A machine carries the taints its configuration
declares, and while they last, `on-battery`, `memory-pressure` (past
its queue's memory limit) and `disk-pressure` (its `disk_band` past
`healthy`), each `prefer_not`. A machine qualifies when it is in reach,
leaves after the work ends (now, for work that doesn't say how long it
lasts), is what each need asks (a fact it never said meets none), and carries
no `never` taint the work doesn't tolerate. Of those, one with no
`prefer_not` taint goes first, then the most preferences met, then the
most memory free, then by name. The answer is
`{"placed": {"machine", "passed"}}`, `{"pending": {"misses"}}` when
only a machine out of reach would do and the work may wait, or
`{"refused": {"misses"}}`; each miss names a `machine` and `because`:
`{"lacks": need}`, `{"unknown": need}`, `{"refuses": taint}`,
`{"rather_not": taint}`, `"ranked"`, `"unreachable"` or
`{"leaves_first": time}`. Placement only
chooses: a machine says whether work starts there when asked, by its
own queue and taints, so nothing fits work against its room, which only
ranks it.

`"Start"` places `work` as `"Place"` does and starts `command` held
there through the person's login shell (`$SHELL -lic`), so it has their
environment rather than the node's. It begins in `folder`: a full path
on that machine, or a path within its home, such as `src/core.nix`,
since each machine's home is its own; `mycelium start --folder` says a
folder under home that way. It begins in its home when `folder` is
none or names no folder there. The answer is the `machine`, the `held`
id, the `folder` it began in, and `attach`, the words that join it from
a terminal on the asking machine, to be run as given. A start that
isn't placed, or doesn't start, is an error saying why, such as
`nowhere: optimus isn't equilibrius`. A node run as a systemd unit
starts each holder in a scope of its own, `mycelium-pi-<held>.scope` in
`app.slice`, so the session outlives the node. A start placed on
another machine is asked of it through the link this node keeps to it,
and its `attach` comes back as the words that run that machine's own
over the dial, such as `ssh -t equilibrius mycelium --realm work attach
<held>`. It fails with a reason when no link reaches the machine, its
mycelium is too old to start sessions, or it doesn't answer within 30
seconds.

`"Steer"` puts `steer`, in the harness's own shape (`text`, an `act` of
`pause`, `resume` or `stop`, `at`, `from` and a talk's `id`), into the
steer inbox of the session going by `address` on the machine it runs on:
`~/.local/state/pi/agentic-harness.pi/steer/<ADDRESS>/`, or the folder
`--steer` names, under a temporary name renamed to end in `.json`. A
session on another machine is steered through the link reaching it. A
talk's file, `<ADDRESS>/replies/<id>.jsonl`, is opened on the asking
machine first, and the lines its session writes are carried back to it,
so a talk reads the same wherever its session runs. It is an error,
saying why, when no session goes by `address` now, as `no session goes
by SAK9FZ now`, when its machine is out of reach, when it runs on two
machines at once, or when the link can't carry it.

A watch sends the whole state first, `SessionsChanged` and
`MessagesChanged`, then each change: `SessionChanged`, `SessionForgotten`
and `Message`. A client that falls too far behind is let go, and connects
again for the whole state.

A group is named by dot-separated tokens: the mesh's `#quest/4uhc5q` is
`quest.4uhc5q`, and a peer's `@4UHC5Q` is `peer.4uhc5q`. A pattern takes
`*` for any one token and a final `>` for one or more.

### The Daemon Interface

A client that opens with `{"id": 1, "ask": "hello"}` speaks version 1
of the daemon interface instead, which every daemon on a machine speaks
alike; any other first line keeps the requests above. Each ask is a JSON
line with the client's `id`, and every answer to it carries that `id`, so
several asks share one connection. A direct answer also carries `took`,
in milliseconds.

| Ask | Answer |
| --- | --- |
| `hello` | `version`, `domain` (`node`), `state` and `kinds`, each with its `live` fields |
| `get` with `keys` | `records`, those held, in the order asked |
| `list` with `kind`, and `limit` | `records`, every one of the kind |
| `watch` with `kind`, and `keys` | `records` whole, then `changed` and `gone` |
| `unwatch` | nothing; the watch with that `id` ends |
| `health` | `state` and each linked machine as a source |

A record carries `key` (`session:<id>` or `machine:<name>`), `kind`,
`heard` (milliseconds since the epoch) and the fields `Sessions` and
`Machines` give it. A session also says whether it runs `here`. The
`machine` kind holds the realm's other machines only, since this machine's
own readings are its machine daemon's. A watch sends what changed by
key, with only the fields that differ, a dropped one as null; nothing
while nothing changes; and no sooner than 250 ms after its last send, so
a reader too slow for it is sent the latest rather than a backlog. An ask
it can't answer gets `{"id", "error": {"code", "message"}}`, the code
one of `unknown-ask`, `unknown-kind` or `not-an-ask`.

## A Held Session

`mycelium hold -- <program>` starts a program on a terminal that a
holder of its own keeps, so the window it started in can close and
`mycelium attach <id>` take it up again from any terminal on the
machine. The holder is its own process, in a session of its own, rather
than the node's: restarting the node's service stops everything the node
started, and a closing window hangs up on its own session only.

Each holder serves `<id>.sock` in `<runtime>/mycelium/<realm>.held`,
reachable only by its account, beside
`<id>.json`, which says what it holds: `id`, `command`, `folder`, the
`holder`'s and the program's `pid`, `since`, and the `windows` joined to
it that said which they are, the latest last. A window leaves the record
the moment its terminal leaves, and both go when the program ends.
`mycelium held` lists every record whose holder still runs. The program
is told its id in `MYCELIUM_HELD`, and loses the variables naming
the window it started in.

A joined terminal sends frames of a tag byte, a two-byte big-endian
length and what it carries: tag 0 is bytes typed, and tag 1 is its size,
rows then columns, two bytes each, sent on joining and on each change.
Tag 2 is the window the terminal is, as JSON in a session's `window`
shape, once on joining when it can say: `mycelium attach` in a kitty
window says its id and kitty's socket.
The holder sends back the program's output as it comes. Any number may
join at once; the program takes the latest size it was told. One joining
at the size the program already has makes it one column narrower first,
so the program draws itself again. Leaving is closing the connection;
the holder closes it once the program has ended.

## Coming Next

Each lands behind a seam above, or as a seam of its own:

- **The node's own log:** messages and presence numbered per machine, so
  catching up is one question, "what do you have from machine X after
  number N?", in place of the mesh's folders.
- **Places and what they can do:** a machine says what it is, and the
  node places work and starts it held on whichever machine it chose,
  which admits or refuses it on its own facts. The launcher and pi
  already start sessions through it; next, a place says what it offers,
  such as evaluating and building for a system, so work that needs it is
  placed there.
- **Providers:** programs beside a node that offer shapes of machine,
  make one, start a session on it, reach it and end it with its cost,
  and say when one is lost, such as Aquifer's, which lives outside this
  repository. A machine a provider made shows as a place like any other.
  A machine that can't dial out is dialled in, through its provider's
  program (`--via`, above), reaching a node that answers links on a port
  only its own machine can reach, which is next.
- **Connections of the node's own:** machines dialled by key, found on the
  network without a cloud, and encrypted always, behind `Dial` and
  `Discovery`; membership then checks the realm's enrolled keys. SSH
  and providers' programs stay the only dials until per-pair keys become
  the burden, or SSH itself makes a link miss 5 ms.
