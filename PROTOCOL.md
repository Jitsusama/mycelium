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
| One writer | a session's presence | only its own machine writes it; others hold copies |
| Shared files | a shared quest's record | each machine holds the same, settled as below |
| This machine's own | read markers, the queue, what is said to `#machine`, held sessions, what the machine is | never |

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

1. `{"Hello": {"machine", "realm", "version"}}`, first, from each end.
   Each end's membership admits the other or refuses it, and nothing else
   crosses until it has.
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
4. `{"Presence": {"session", "body", "leases"}}`: a living session of
   the sending end's machine, again whenever it or its leases change.
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

Three seams shape a link:

- **Dialling** (`link::dial::Dial`): how a machine reaches another's end.
  Today, SSH: the other end is `mycelium link` run there, over the SSH
  session's pipes, and SSH has proved which machine it is.
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

Each node serves one unix socket, `$XDG_RUNTIME_DIR/mycelium/<realm>.sock`
on Linux and the account's temporary folder on macOS, reachable only by
its account. It speaks as niri's does: a client writes one request as a
JSON line and reads one reply, `{"Ok": ...}` or `{"Err": "why"}`.

| Request | Reply |
| --- | --- |
| `"Sessions"` | `{"Sessions": [session]}`, this machine's and others' |
| `"Machine"` | `{"Machine": machine}`, this machine as measured now |
| `{"Messages": {"groups", "limit"}}` | `{"Messages": [message]}`, oldest first |
| `{"Send": {"from", "groups", "intent", "text", "re"}}` | `{"Sent": message}` |
| `{"Watch": {"groups", "recent"}}` | `"Watching"`, then events |

A session's `state` is `busy`, `idle`, `ended`, or `unreachable` while no
link reaches its machine. Beside its `id`, `address`, `machine` and
`groups`, it carries what its presence says of it: its `quest` by id (or,
from an older presence, the quest among what it owns), what it `wants` of
its person (`kind`, `since`, `said`) while it waits, and the `window` it
runs in (`terminal`, `id`, `within`, such as a kitty window's id and
kitty's socket). Only a session of this machine's has a `window`, since a
window is gone to on its own machine. A session of this machine's that a
holder keeps, as one started with `mycelium hold -- pi` is, says `held`:
the id `mycelium attach` takes it up by. It is known by the holder's
program being the session's own process, and its `window` is the latest
window joined to its holder that is still joined, or none. Its
`presence` goes along whole, as its session wrote it, for a reader that
shows more of it, such as the launcher; a session elsewhere's has its
process taken out. Whether a session lives is its `state`, never
anything in its presence.

A machine is what its node measures of it when asked, the facts work is
placed by: its `name`, `measured_at`, `state` (`ready` for a node answering
for itself), `os`, `arch`, `model`, `cpu` (`model`, `cores`), `memory` and
`disk` in bytes (the disk holding its account's home), and `accelerators`
(each a `kind`, `gpu` or `npu`, and its `driver`). A fact that can't be
read is `null`, never a guess. It stays on this machine's socket: no link
carries it yet, nor what must be declared or changes by the minute.

A watch sends the whole state first, `SessionsChanged` and
`MessagesChanged`, then each change: `SessionChanged`, `SessionForgotten`
and `Message`. A client that falls too far behind is let go, and connects
again for the whole state.

A group is named by dot-separated tokens: the mesh's `#quest/4uhc5q` is
`quest.4uhc5q`, and a peer's `@4UHC5Q` is `peer.4uhc5q`. A pattern takes
`*` for any one token and a final `>` for one or more.

## A Held Session

`mycelium hold -- <program>` starts a program on a terminal that a
holder of its own keeps, so the window it started in can close and
`mycelium attach <id>` take it up again from any terminal on the
machine. The holder is its own process, in a session of its own, rather
than the node's: restarting the node's service stops everything the node
started, and a closing window hangs up on its own session only.

Each holder serves `<id>.sock` in `$XDG_RUNTIME_DIR/mycelium/<realm>.held`
(the temporary folder on macOS), reachable only by its account, beside
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
- **Places and what they can do:** a machine says on its socket what it
  is today; next, what is declared of it in its configuration (its
  reach, the reserve kept for its person, the refusals it enforces) and
  what it can take on now, carried across links as sessions are. Work
  says what it needs and prefers, and placement answers placed, pending
  or refused, naming each place and the need it failed. A place admits
  or refuses work itself, so a placer's view can't override it.
- **Providers:** programs beside a node that offer shapes of machine,
  make one, start a session on it, reach it and end it with its cost,
  and say when one is lost, such as Aquifer's, which lives outside this
  repository. A machine a provider made shows as a place like any other.
- **Connections of the node's own:** machines dialled by key, found on the
  network without a cloud, and encrypted always, behind `Dial` and
  `Discovery`; membership then checks the realm's enrolled keys. SSH
  stays the only dial until a machine has no SSH path, per-pair keys
  become the burden, or SSH itself makes a link miss 5 ms.
