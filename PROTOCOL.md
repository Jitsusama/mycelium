# Protocol

What mycelium keeps, what crosses between machines, and how a client
speaks to a node. Each seam below is a trait in the code, with one
implementation today and room for the next.

## Words

- **Machine:** one computer, whether Joel's, one on his network or one in
  a cloud. Every machine runs the same node.
- **Node:** mycelium's program, one per machine and realm.
- **Realm:** a set of machines that link, such as work or personal.
  Nothing crosses between realms, not even presence, but the text a
  flow lets through one way (Realm Flows, below).
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
a hold before skiff reboots means nothing to the Mac's sessions.

A machine has one mesh, so it belongs to one realm, recorded in
`~/.local/state/mycelium/realm` the first time a node or link runs there;
another realm is refused, so nothing of one realm reaches another but the
text a flow lets across (see Realm Flows).

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
   "steers", "releases", "updates"}}`, first, from each end. Each end's
   membership admits the other or refuses it, and nothing else crosses
   until it has. `facts` says the end hears and says what a machine is; an end
   older than it says nothing of machines and is sent nothing. `starts`
   says it carries starts, and `steers` steers; an end older than either
   is asked none of it. `releases` says the end's way to the other
   end's machine can release that machine, as a provider's program can;
   only such an end is told the machine has handed over. `updates` says
   it carries update asks; an end older than it is asked none.
   `machine` is the name the end's machine goes by, one name for every
   command there: the node's `--machine`, else the system's name up to
   its first dot, which the node records in its folder's `machine` file
   as it starts. Any other command takes its own `--machine`, else that
   record, else the system's name, so a link the other end starts with
   no `--machine` goes by the node's name. A name that isn't a mesh name
   (letters, digits, `-`, `_` and `@`), as a guest's `(none)` isn't, is
   refused, saying to pass `--machine`.
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
   with the presence. An end older than `leases` sends none. A session
   carried from one machine to the other keeps its id, and its first
   machine keeps its file after it ends there: that file, ended, gives
   way to the presence from where it runs now, and its holding with it,
   while one whose process still lives there never does.
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
10. `{"Steer": {"ask", "address", "steer", "checked"}}`: put what the
    person says into the inbox of the session going by `address` on the
    other end's machine, as a `"Steer"` request says it. The other end
    writes it there itself, since a steer is only a file. `checked`,
    left out when false, says the sending end's node saw the person's
    own process ask; without it the other end refuses a steer that
    claims the person, saying so. A steer with an `id`
    opens a talk, which the other end follows from then on. Words with
    `"now": true` go into the session's turn under way; without it,
    they wait for that turn to end.
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
    there or a program held there, such as a build; a start this end
    answered counts as some having run, however soon it ended. The
    sender first says all
    it holds of the quests it shares. Frames cross in order, so if the
    other end then waits on no file from it, it holds all of them: its
    link ends, and its node releases the machine and dials it no more.
    An end older than this never hands over, so it is never released.
14. `{"Update": {"ask"}}`: update the other end's machine to its
    configuration's main, as an `"Update"` request says it, answered by
    `Started`. The other end takes it only if its membership proved
    which machine asked, as a pinned link key does, and hands it to its
    own node's socket; otherwise it refuses, saying why. No time limits
    the answer, since an update takes minutes; a switch there restarts
    the link, which then ends the wait.

Only one link joins two machines, and whichever dialled first holds it,
so each end carries what its own node asks of the other machine: starts,
steers and updates. The dialling end runs in its node and takes them
there. The dialled end runs in a process SSH started, outside its node,
so it holds a `Carry` connection to its node's socket (see The Socket),
asked again every five seconds while the node can't be reached. Its node
joins a session there, or one it started, over the way its own dial to
that machine reaches it, or else over SSH by the machine's name.

Three seams shape a link:

- **Dialling** (`link::dial::Dial`): how a machine reaches another's end,
  and how a terminal here runs a command there. SSH (`--ssh`): the
  other end is `mycelium link` run there, over the SSH session's pipes,
  and SSH has proved which machine it is. A machine's link key may be
  pinned in the other's `authorized_keys` to `mycelium --realm REALM
  ssh --from NAME`, which reads what was asked from
  `SSH_ORIGINAL_COMMAND` and runs only `mycelium link --realm REALM`,
  `mycelium --realm REALM attach ID` (or `holds ID attach`, as it is
  spelt now) or `mycelium --realm REALM attach --host HOST --session
  ID`, each in plain words. That link
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
- **Membership** (`link::membership::Membership`): which machines to admit,
  and whether the far end is proven to be the machine it says. Today, a
  realm: any machine of the same realm speaking the same version, never
  the machine itself, proven only when a pinned key let the link in. A
  machine refused says why, naming both versions and which machine to
  update when they differ, and the node keeps it until a link is
  admitted, so `health` gives it as the link's why.

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
`node_modules/` stay home. A folder once named stays shared. The
scratch's `handover/` crosses whenever it is there as the quest is
shared, named or not, under the same limits, since a carry or a spawn
leaves there what the other machine needs to start. Sharing a quest
shares each quest above it too, up its README's `parent` while each is
on this machine, as quests of their own with none of their scratch
named, so a session on the other machine can tell what orchestrates it.

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
as `Share` frames. A look walks a quest's record, or a named scratch
folder, only when a watch heard it change, when it is first looked at or
newly named, or when it can't be watched; a watch that fails or
overflows has every record and folder walked. A change found while
hearing the other end is said at the next look:

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
  names it, that end takes its own scratch away, as concluding did
  where the quest ended, then stops sharing it and isn't taken up
  again, its record left as it is. A scratch where a process has its
  working folder or an open file is let be, with the quest still
  shared, and tried again a minute on; one that can't be removed is
  left and said. Linux's `/proc` says what works where, and `lsof`
  does on a Mac; where neither answers, the scratch counts as in use.
  An end older than `ended` names none, so a quest stays shared with
  it.
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
marked, so a path under `/home/jitsusama` reads `/Users/jsama` on a
Mac. Each write takes the quest's lock as the harness does and lands by
rename, so a session never reads half a file.

Each end keeps, in the quest's ledger as `theirs`, what the other said
it holds in its `Holds`, and what it sent and was seen to take: an end
that takes a file says `Holds` of it even when it now holds just what
was sent, so the sender knows. Only files an end holds itself are kept
there, so a file deleted drops out.

`mycelium share QEST-... --to <machine> --wait` returns only once that
machine's node holds the quest, so a session started there after finds
all it needs. It counts every file of the record and of the named
scratch this end holds, and the README of each quest above it, named by
its quest as `QEST-.../record/README.md`, at the version its link's next
look will say, from a copy of the ledger it never keeps, and holds once
the machine holds each at that version or newer. A git clone and a deletion aren't
waited on. It looks four times a second, as long as `--within` says (120
seconds unless told), and prints one JSON line, as the harness's spawn
reads it:

- held, exiting 0: `{"quest", "machine", "held": true, "files",
  "holding", "scratch", "waited"}`, `files` counting what this end
  holds, `holding` what the machine holds of it, `scratch` the folders
  shared and `waited` its seconds;
- not held, exiting 1: the same with `"held": false`, up to 20 of the
  files it lacks as `lacking`, and a `reason`, with `why` in words, also
  said on stderr: `refused` when the quest couldn't be shared or the
  machine is this one, `no-link` when no link has reached the machine
  for 5 seconds, `not-shared` when its node hasn't said it shares the
  quest within 5 seconds of a link reaching it, and `behind` when it
  still lacks a file at the bound. An end older than `theirs` never says
  it took a file, so a wait for it ends `behind`.

## Carrying a Quest

`mycelium carry QEST-... --to <machine>` takes a quest's work and its
sessions to another machine of the realm, as the person moves from one
laptop to another. The harness does what only it knows; the node puts it
in order and says each step:

1. `find`: the quest's sessions on this machine that can carry their own
   work, those running and those parked this boot. A machine not in reach,
   or this one, is refused.
2. `push`: each is steered `{"act": "carry", "to": <machine>}`; the
   harness puts every tree it holds and the session's file into the
   quest's scratch, under `handover/<this machine's short hostname>/`
   (up to its first dot), writes
   `<session>.done.json` there saying what it put (at least `session`;
   the harness adds its folder and trees, within its home), and ends.
   Once each has, `pi-carry QEST-... --to <machine>` carries what no
   session held.
3. `share`: the quest is shared with `handover`, as `mycelium share
   --scratch handover` does.
4. `start`: each carried session starts again on the machine, held, as
   `mycelium start` starts one, running `pi-carry arrive QEST-... --from
   <this machine> --session <id>`, which waits for the handover, fetches
   or clones each tree at its path and places the session's file, and
   then `pi --session <id>` in the folder `pi-carry where QEST-... --from
   <this machine> --session <id>` prints: its own where a tree it brought
   holds it, else its quest's scratch, so it never starts in a folder of
   the person's that no tree brought. That folder is entered as it
   stands, never made. Its first prompt
   saying where it came from and telling it to carry on where it left
   off, so it never waits to be woken. The quest's own done
   file, `<quest>.done.json`, holds the trees no session held; it
   arrives the same way, but no pi starts for it and it isn't among the
   sessions said. Once each has started, `<session>.started` is written
   beside its done file, which stays for `pi-carry` there to read, and
   no carry takes a done file up again until it is written afresh, newer
   than its mark, so neither a later carry nor this one taken again
   starts a session twice.
5. `close`: the sessions here are seen to end.

Each step prints `{"step", "state", "said"}` on a line as it begins
(`doing`) and ends (`done`, `skipped` or `failed`), and the last line is
`{"step": "carried", "state": "done", "machine", "sessions"}`, each
session `{"address", "held", "attach"}`. A step that fails says why, the
command exits non-zero, and nothing after it runs; what was done stays
done, since each step can be taken again. It waits 15 minutes in all for
the sessions to carry and then to end.

### A Carry an Orchestrator Orders

An orchestrator carries a session of a quest it orchestrates with a steer
`{"act": "ordered-carry", "to": <machine>, "why": <reason>}`, an act of
its own, so nothing trusting the person's `carry` trusts it. The node
takes it only when it proves, from its own record, that the session
asking orchestrates that session's quest, never from what either
session says of itself:

- Who asks is the client's process, as the kernel names it: its line of
  parents is walked to the pi owning a session running here. A session
  writes its own presence, so its `owner` is believed only where records
  it doesn't write back it: the process `owner.pid` started at
  `owner.startTicks`, by the kernel's `stat`, and either pi's host keyed
  `owner.host` lists the session in its own record,
  `hosts/<pid>-<startTicks>.json` in pi's state, or no host holds it and
  the process is pi's own, with no other session claiming it. Either
  way, that process must hold the session's log open, as pi does each
  it owns: `<time>_<session>.jsonl` in a folder of pi's `sessions`, by
  the real path the kernel gives in `/proc/<pid>/fd`. Any process of
  the person's account may open that file, so this keeps one session
  from being taken for another by confusion, not by a helper set on
  it: the fence is the boundary against that. One filed
  under another session's id owns nothing, and two presences claiming
  one session refuse the carry outright. Where one pi hosts several sessions,
  the asker's own `PI_SESSION_ID`, read from its process, says which,
  and it must be one of them. Where the kernel can't say when a process
  started, as on a Mac, the ask is refused.
- Who orchestrates is the node's own record, `orchestrations.json` in
  mycelium's folder, of what each orchestrator told it with
  `{"Orchestrates": {"heads", "quests"}}`: the session asking, proved as
  above, heads the orchestrating quest `heads` and orchestrates
  `quests`, in place of what it said before. Pi says it as a session
  loads an orchestrating quest and whenever the quests below it change,
  and says it with neither as it unloads or concludes, which forgets
  what that session held. A quest another session holds is refused:
  only that session hands it on, by forgetting it, or by being
  continued, as a session whose log's first line, `<time>_<id>.jsonl`
  under pi's `sessions` folder, names it as `parentSession`. What a
  session claimed lapses once it neither runs nor waits parked, as the
  record is next read or changed, so an orchestrator started afresh
  claims its quests again and one ended holds nothing; and the person's
  own login, never an agent's, frees a quest from whichever session
  holds it with `{"Free": {"quest"}}`, whole where that session heads
  it. The file is the node's state, never written by hand; a fenced
  session is kept from writing it only by its fence. The
  session's quest is the one whose `sessions` holds its id in the
  quests' record the node serves as `quest`, and the asker must hold
  that quest, or the quest the one holding it heads, and so on up. A
  quest no orchestrator told the node of is the person's alone to carry.
  A presence's `quest`, `orchestratedBy` and a pi's `PI_QUEST` are claims
  and count for nothing.
- The chain is believed only where the quests' record gives the same
  one, by each quest's `orchestrator` address, and the mesh agrees:
  every running or parked session of each quest in it has its
  orchestrator's `#orchestration/<address>` among its presence's
  `groups`, at least one does for each, and so does the asker for its
  own. Pi works both out from the quests' front matter, so they check
  the node's record and never prove it.
- The ground it lands on is at least as firm: `to` is another of this
  realm's machines, in reach, whose `declared` `fence` runs sessions
  in the fence the carried session's presence names as its `fence`
  (`none`, `fence` or `strict`), and this machine's own declaration
  runs that fence too, with no door of it there wider than here: each
  of its `builders`, `forges` and `secrets` is one here's opens. A
  session that names no fence, or a machine that declares none for it,
  is refused, since where it lands can't be told. A refusal names the
  doors that are wider, as `tern's strictly fenced session opens
  builders cloud, which here's doesn't`.

A refusal says why and who could order it, as `OSPHT4 doesn't
orchestrate QEST-... by this node's record, so it can't order its
carry: only WCJQSG's session could`. Taken, the steer names the
orchestrator's address as `by` and `from`, whatever the client gave, so
it never reads as the person's, and before it reaches the inbox it is
kept as a JSON line in `carries.jsonl` in mycelium's folder: `at`, `by`,
the `asker`'s session, the carried `session`, its `address` and
`quest`, the machine it goes `from` and `to`, `why`, and the `chain` it
was taken on, each quest with its `orchestrator`, nearest first, with
the `source` that chain came from, so a forged one is traced. It is proved
only where both sessions run: one on another machine is refused, and a
node never takes one a link carries. As the person's own steers, this
keeps an agent from ordering a carry by habit or mistake, not one set
on it: every record here is a file the person's account writes, so it
holds against a fenced session only where the fence keeps it
read-only.

## Realm Flows

Realms are kept apart, but a machine's configuration may open a flow: a
one-way road for text from one realm into another, such as personal
into work, which never opens its reverse. A realm's node is given the
realms its messages may go to (`--flow-to`) and those it takes from
(`--flow-from`); a crossing happens on a machine running a node of
each, between those two nodes, and nowhere else.

Only a message whose sender names the other realm crosses, sent to the
node's socket as a `Send` with a `realm`; nothing crosses by a guess
from an address no session here has. It crosses as text alone: its
`from`, `groups`, `intent`, `text` and `re`, under the id it was sent
with, and only as a `fyi`, `finding` or `question` to peers by address
(`peer.16v6f4`), since a hold or a pivot changes what other sessions do
and a group such as a quest's or `#machine` belongs to its realm. No
share, start, steer, presence, lease, machine or file ever crosses.

Both ends judge it. The sending realm's node hands a message across only
under its flow to the other realm, to that realm's node at
`<runtime>/mycelium/<realm>.sock` as `{"Cross": {"message",
"from_realm"}}`; the receiving realm's node takes it only under its flow
from the first, and only when a session of its realm, on any machine,
goes by an address it names. It writes the message into its realm's mesh
with `"fromRealm"`, so its sender can't pass for a local peer, and its
links carry it on as any message. A message it already holds, as one a
second machine of both realms crossed first, is answered as sent and
not written twice. A crossing made on one machine leaves nothing in the
realm it left.

A machine with a flow but no node of the other realm, as osprey has
none of work's, keeps the message in its own realm's mesh marked
`"forRealm"`, which no session there hears and the node lists to no
client. Links carry it as any message; a node with the flow on a machine
that runs the other realm's node hands it across as it arrives, and
every minute again while that node doesn't answer. One the other realm
refuses is said to its sender in its own realm, as a `finding` from
`mycelium@<machine>` answering it. A message waits at most the mesh's
week.

Nothing crosses back, an answer included: a crossed message can't be
answered on the mesh, since an answer is still its realm's text. Each
node keeps every crossing it judged, taken or refused, as a line of
`crossings.jsonl` in its own folder: `at`, `from_realm`, `to_realm`,
`from`, `to`, `intent`, `id`, `taken`, and `refused` (`code`, `why`)
for one that wasn't. A message judged for good there isn't handed across
again when the node restarts.

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
| `"Offers"` | `{"Offers": {"offers", "unanswered"}}`, asked of each provider now |
| `{"Place": work}` | `{"Placement": placement}`, where the work goes |
| `{"Start": {"work", "folder", "command", "may_cost"}}` | `{"Started": {"machine", "held", "folder", "attach"}}` |
| `{"Steer": {"address", "steer"}}` | `{"Steered": {"machine"}}`, where it was put |
| `{"Orchestrates": {"heads", "quests"}}` | `"Orchestrated"`, recorded (A Carry an Orchestrator Orders) |
| `{"Free": {"quest"}}` | `"Orchestrated"`, freed, from the person's own login |
| `{"Update": {"machine"}}` | `{"Updated": ending}`, as its updater said it |
| `{"Messages": {"groups", "limit"}}` | `{"Messages": [message]}`, oldest first |
| `{"Send": {"from", "groups", "intent", "text", "re"}}` | `{"Sent": message}` |
| `{"Send": {"from", "groups", "intent", "text", "re", "realm"}}` | `{"Sent": message}`, or refused |
| `"Flows"` | `{"Flows": {"to"}}`, the realms a `Send` may name |
| `{"Cross": {"message", "from_realm"}}` | `{"Sent": message}`, or refused |
| `{"Watch": {"groups", "recent"}}` | `"Watching"`, then events; no `groups`, no messages |
| `"Present"` | `"Present"`, then held open while its process runs |
| `{"Carry": {"machine"}}` | `"Carrying"`, then what it asks of `machine` |
| `{"Turn": {"session"}}` | `"Turn"`, once a holder may resume `session` (A Held Session) |

A `Send` naming another realm, and a `Cross`, may be refused by a flow,
answered `{"Refused": {"code", "why"}}` beside `Ok` and `Err`: `why` is
one line for the sender, and `code` one of `no_flow` (no flow that way),
`not_text` (not a `fyi`, `finding` or `question` to peers by address),
`unheard` (no session of the other realm goes by an address it names)
and `unreached` (the other realm's node here doesn't answer). A message
sent across carries `fromRealm`, and one left waiting for a link to
carry it `forRealm`. A `Send` naming its own realm is a plain send.
A message answering a hold (`re`) on the person's word that it is over
carries `endsHold`, kept as its sender's file says it, and nowhere else.

A `Present` connection counts the process at its other end, as the
kernel names it, present until the connection closes; then every session
that process owns is ended at once. It says nothing more, and a later
`Present` from the same process takes its place, so the earlier one's
close ends nothing. A process no longer there, or one that is present
again, is let go of. A node that can't tell which process connected
refuses.

A `Carry` connection is how the end of a link another machine dialled
carries what its node asks of that machine. The node writes each ask as
`{"ask", "asked"}`, where `asked` is `{"Start": start}`, `{"Steer":
{"address", "steer", "checked"}}` or `"Update"`, and the link writes back
`{"ask", "reply"}` with the other end's reply. It lasts until either
hangs up, or until another link reaching the machine takes its place;
an ask still waiting then hears that the link dropped.

A session's `state` is `busy`, `idle`, `ended`, or `unreachable` while no
link reaches its machine. Beside its `id`, `address`, `machine` and
`groups`, it carries what its presence says of it: its `quest` by id (or,
from an older presence, the quest among what it owns), what it `wants` of
its person (`kind`, `since`, `said`) while it waits, the `landings` it
says it made lately, newest first (each `at`, `ref`, a one-line `what`
and, when it means something to its person, `person`; any it can't read is
left out), what its plan is `doing` (its `plan` by id, the `item` it works
on by number where it says one, the `next` thing it does and what it
`chose` last where it says, each a line of at most 120 characters, and
the `at` it said so; a `doing` it can't read, or whose line is longer, is
left out whole), its latest `events`, newest first and at most 20 (each
`at`, its `kind` as `commit`, `push`, `pr`, `merge` or `stage`, `where`
as a repository's key or a quest's id, and a `title` cut to 120
characters; any it can't read is left out), and the `window` it
runs in (`terminal`, `id`, `within`, such as a kitty window's id and
kitty's socket, and `app_id`, the app id its window was opened with,
where its opener said so in `MYCELIUM_APP_ID`, which names the same
window in the compositor's list). Only a session of this machine's has a
`window`, since a window is gone to on its own machine. A session a
holder keeps, as one started with `mycelium holds add -- pi` is, says
`held`: the id `mycelium holds ID attach` takes it up by on its
machine. It is
known by the holder's program being the session's own process, which
only its own machine sees, so a link carries it with the presence. A
held session of this machine's has as its `window` the latest window
joined to its holder that is still joined, or none. A held session says
`attach` too, the words that join it from a terminal on this machine, to
be run as given: `mycelium` here, or the dial's words for a machine a
link of this node's reaches, and none while no link does. A session
that runs among others in one pi host, whose presence says `hosted`
(`host`, the host's own key, and `conversation`, the id that outlives
the host's restarts), says its `host` and `conversation` too, and is
never `held`: its process is the host's, which many share, so the
host's holder is none of its own. A part of `hosted` it can't read is
left out. Its `attach` is `mycelium --realm REALM attach --host HOST
--session CONVERSATION`, or the dial's words for it elsewhere, which
joins it through its host's socket in the login's runtime folder,
`pi-host/HOST.sock`; closing that terminal only leaves, and a frame of
a kind it doesn't know is passed over rather than ending the join. Its
`presence` goes along whole, as its session wrote it, for a reader that
shows more of it, such as the launcher; a session elsewhere's has its
process taken out. Whether a session lives is its `state`, never
anything in its presence.

A machine is what its node measures of it when asked, the facts work is
placed by: its `name`, `measured_at`, `state` (`ready` for a node answering
for itself or a machine a link carries, `unreachable` for one no link
reaches now, as of when one last did), `os`, `system` (`linux` or
`macos`, `null` from a node too old to say it), `arch`, `model` (a
Mac's is its identifier, such as `Mac14,2`), `model_name`, its kind in
words (`MacBook Air`, absent from a node too old to say it), `cpu`
(`model`, `cores`), `memory` and `disk` in bytes (the disk holding its
account's home), `disk_free`, how much of that disk its account may
still write, in bytes (absent from a node too old to say), `disk_band`,
how full that disk is as the machine's disk guard bands it (`healthy`,
`filling`, `nearly-full` or `critically-full`, from the guard's
`status.json` while its heartbeat is under two minutes old, and absent
otherwise; the guard's earlier `pressure`, `urgent` and `critical` are
read as those three, and a band a node doesn't know is read as absent),
`accelerators` (each a `kind`, `gpu` or
`npu`, and its `driver`), and `power`: `on_battery`, true only with a
battery of its own and nothing plugged in, and `charge`, its own
batteries' mean in percent, `null` for a machine with none. A fact that
can't be read is `null`, never a guess. Its `updated` is how its last
update to main ended, as its updater keeps it in
`/run/update-to-main/last.json` (`/var/run/update-to-main.last.json` on
macOS), and absent where none is kept, as after a reboot. It carries `at`,
when the update ended: the updater's own where it says one, else when it
last wrote that file, to the second.

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
A machine on its battery below 20% is let sleep, so its node puts the
same kind of steer, once while it stays below, saying to commit and push
and put the rest where its work is shared, though not to stop, since a
session waits out a sleep; once it charges, the next time it runs low
says so again.
Where given, `within` names the machine it runs within, as a virtual
machine does its host, and a file the two share, `{"machine": "...",
"file": "..."}`; its `power` and `room` are then its host's, as the host
last said them there, while that is under two minutes old, since it
reads neither of its own truly. A host declares each machine within it
in `hosts`, in the same shape, and its node writes its `power` and
`room` to each file each minute, whole.
Where given, `fence` says the fences an agent session runs in there, by
kind (`none`, `fence` or `strict`), each with the doors it opens:
`{"strict": {"builders": [...], "forges": [...], "secrets": [...]}}`,
each door by the name the machine's configuration gives it. A fence
missing a door, or of a kind this build doesn't know, is read as no
fence, spoiling nothing else declared; a carry an orchestrator orders
lands only where it says one (A Carry an Orchestrator Orders). Only
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

`"Offers"` asks each program the node was given as `--provider PROGRAM`
what it could make, all at once, as `PROGRAM offers`. The program writes
one JSON line a shape and ends within 30 seconds:
`{"shape", "facts", "costs": {"hourly", "currency"}, "lasts"}`. The
`shape` is the program's own name for it; `facts` says what such a
machine would be, in a machine's own words (`system` such as `linux`,
`arch` such as `x86_64`, `cores`, `memory`, `disk` and `declared`),
and anything left out is unknown; the
hourly cost is a decimal written out, such as `"0.12"`, in a
three-letter currency; `lasts` is the longest one lives, in seconds,
where its provider fixes it. Each offer comes back with the `provider`
that offered it, and a program that couldn't say is in `unanswered`
with why, rather than taken as offering nothing. An offer is the
provider's promise, never a fact about a machine: it only decides
whether one is worth making, and once made, the machine's own node says
what it is.

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
`healthy`), each `prefer_not`, and `low-battery` (on its battery below
20%, when it is let sleep), which is `never`. A machine qualifies when
it is in reach, leaves after the work ends (now, for work that doesn't
say how long it lasts), is what each need asks (a fact it never said
meets none), and carries no `never` taint the work doesn't tolerate. Of
those, one with no `prefer_not` taint goes first, then the most
preferences met, then the most memory free, then by name. The answer is
`{"placed": {"machine", "passed"}}`, `{"pending": {"misses"}}` when
only a machine out of reach would do and the work may wait, or
`{"refused": {"misses"}}`; each miss names a `machine` and `because`:
`{"lacks": need}`, `{"unknown": need}`, `{"refuses": taint}`,
`{"rather_not": taint}`, `"ranked"`, `"unreachable"`,
`{"leaves_first": time}` or, for an offer, `{"lives_only": duration}`.
Placement only
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
`nowhere: skiff isn't kestrel`. A node run as a systemd unit
starts each holder in a scope of its own, `mycelium-pi-<held>.scope` in
`app.slice`, so the session outlives the node. A start placed on
another machine is asked of it through the link this node keeps to it,
and its `attach` comes back as the words that run that machine's own
over the dial, such as `ssh -t kestrel mycelium --realm work attach
<held>`. It fails with a reason when no link reaches the machine, its
mycelium is too old to start sessions, or it doesn't answer within 30
seconds.

A start that no machine of the realm takes, and that says it
`may_cost` (`mycelium start --may-cost`), has a machine made for it.
The node asks its providers what they offer and judges each offer as
it would a machine, by the facts it promises: a fact it doesn't promise
meets no need, and none is a machine already named. Of those that would
take the work, it makes one of the cheapest an hour, after taints and
preferences, as `PROGRAM make SHAPE --lasting SECONDS`: the work's
`lasts` and ten minutes to hand over, though no longer than the offer
lives, or as long as the offer lives where the work doesn't say. It
makes none where neither says, since no machine is made without an
end. The program writes one JSON line, `{"place", "machine", "until"}`:
where it is, as `--via` names it, the name its node will say for
itself, and when its provider ends it whatever else happens. The node
dials it as it would `--via PROGRAM:PLACE`, waits up to five minutes
for it to link, places the work again by the machine's own facts, and
starts it there. It lets the machine go, with `PROGRAM release PLACE`,
once its work has handed over, at `until`, or at once when nothing
started on it. Each make and release is kept as a JSON line in
`made.jsonl` in mycelium's folder, `{"at", "made"}` with the machine,
its place, shape, provider, `until` and hourly cost, and `{"at",
"released"}` with why, so what was made and for how long can be summed.
A start whose providers make nothing says why: each machine's and each
offer's reasons, and each program that couldn't say what it offers.

`"Steer"` puts `steer`, in the harness's own shape (`text`, an `act` of
`pause`, `resume`, `fill`, `stop`, `close`, `carry`, `ordered-carry`,
`move` or `answer`, a carry's `to`, the machine it goes to, with an
ordered carry's `why` and `by` (as A Carry an Orchestrator Orders says),
or a move's, `attended`,
`background` or `pi`, an answer's `want` or `look`, the ask or the look
it answers, with a choice's `value` or, for a want, the person's words
as `other`, `at`, `from` and a talk's `id`), into the
steer inbox of the session going by `address` on the machine it runs on:
`~/.local/state/pi/agentic-harness.pi/steer/<ADDRESS>/`, or the folder
`--steer` names, under a temporary name renamed to end in `.json`. A
session on another machine is steered through the link reaching it. A
parked session counts as running on its machine, since it reads its
inbox as it wakes, so what is said is there before it is woken. The node
there then wakes it, when it parked this boot, by writing its holder's
`<id>.wake`: the holder's it knows, else the one the session's presence
names as `wake`, taken only as a holder's in a realm's `.held` folder
beside its own. A talk's file, `<ADDRESS>/replies/<id>.jsonl`, is opened
on the asking machine first, and the lines its session writes are
carried back to it, so a talk reads the same wherever its session runs.
An `act` of `wake`, alone, as the launcher's Wake asks, puts nothing in
the inbox, since the harness has no such act, and only wakes the session
there, so the launcher wakes one without starting a program to write its
file. A wake for a session on another machine, or where no holder's
file can be written, is refused, saying so. It is an error,
saying why, when no session goes by `address` now, as `no session goes
by SAK9FZ now`, when its machine is out of reach, when it runs on two
machines at once, or when the link can't carry it.

Only the person's own login speaks as the person. A steer whose `from`
names nobody, `joel` or `person`, in any case, is refused, saying so,
when the process asking runs as one of the machine's agents: on Linux,
in a unit of the person's service manager whose name begins
`agents.slice/pi-host-` or `agents.slice/mycelium-pi-`, or in one under
`jobs.slice/` or `offload.slice/`; on Linux or a Mac, below a process
named `pi`, as `/proc` or `ps` tells it. It is refused too when the
kernel names no process at the client's end. Where the process can be
told and is the person's, the node says so to a link carrying the steer
to another machine; where it can't be read, the steer goes here but
crosses no link, since the far end refuses what claims the person
without that word, so it refuses all such steers from a node too old to
say it. A steer from anyone else, such as the node's own
`machine`, goes as before. This keeps an agent from speaking as the
person by habit or mistake, not one set on it, since a process of the
person's account can move itself into any unit of theirs, leave pi's
line by forking twice, or write an inbox itself.

`"Update"` asks `machine` to update itself to its configuration's main:
this one through its updater, and one a link of this node's reaches
directly through that link, which the far end takes only when its key
proved this machine asked. Any process of the node's account can ask
through its socket. The updater listens on `/run/update-to-main/ask.sock`
(`/var/run/update-to-main.sock` on macOS), is asked the line `update`,
and answers a JSON line a step, each with its `step`: `fetching`,
`building` and `switching` while it runs, then one ending, `done`,
`unchanged`, `failed` or `refused`. The node logs each step and answers
the ending whole. A switch may restart the node or its link before the
ending comes back, and then the ask fails, saying so; the machine's
`updated` fact says how it ended.

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
| `hello` | `version`, `domain` (`node`), `state` and `kinds`, each with its `live` fields, and those it offers `deep` |
| `get` with `keys` | `records`, those held, in the order asked |
| `list` with `kind`, and `limit` and `fields` | `records`, every one of the kind |
| `watch` with `kind`, and `keys`, `when` and `deep` | `records` whole, then `changed` and `gone` |
| `unwatch` | nothing; the watch with that `id` ends |
| `health` | `state`, and each linked machine and served file as a source |
| `steer` with `address` and `steer` | `steered` with the `machine` whose session's inbox has it |
| `update` with `machine` | `updated`, the ending its updater said, as `Updated` gives it |

A record carries `key` (`session:<id>`, `machine:<name>`,
`message:<id>`, `team:<address>`, `network:<name>`, `unanswered:<id>`,
`said:<id>` or `event:<session>:<at>`), `kind`, `heard` (milliseconds
since the epoch) and the fields `Sessions`, `Machines` and `Messages`
give it. A field its source named `key`, `kind` or `heard`, such as a
job's own `kind`, is kept under `source` rather than lost beneath the
interface's, beside the record's own `source` where it has one. A
session also says
whether it runs `here`, and how it stands, as `standing`: `running` while
its process runs, here or on a machine heard of this boot; `parked` here,
ended for its holder to start again, its park marked this boot; `closed`
here, ended this boot unparked; and no `standing` for one passed over.
Its `told` is a digest of the record without what only an open launcher
shows (its step, usage and times), so a
change whose records carry no new `told` tells a shut one nothing it
reads. The `machine` kind holds every machine of the
realm, this one as its last minute's measure found it, each saying
whether it is `here`. The `message` kind holds each group's latest 100
messages, so a quiet group's last words stay beside a busy one's, and a
message is heard when it was sent and never changes. The `team` kind
holds each orchestrator's team, keyed by its address: the living
sessions answering to it as `members`, its `lead` working on its quest,
its `title` as a served `quest` names it (else its lead's), the name it
goes by in the swarm as `swarm` where that quest gives one, the team it
sits `within` by its lead's orchestrator, and `counts` of its members
as `needs-you` (wanting their person), `stuck` (paused or parked),
`moving` and `quiet`. Its `worst` is the worst of its own and of every
team within it. Its `bridges` are repositories another team's sessions
work in too, its `liaisons` its sessions hearing another team's
`orchestration.<address>` group, and `landed` its newest landing. A
session answering to nobody and leading nothing is in no team.

The `network` kind holds each network of the realm, keyed by its name
as the mesh names its group, such as `repo.skiff` or `machines`, made
by the very model `mycelium networks` draws, with its fields: its
`shape` (`orchestration`, `project`, `repository`, `group` or
`machines`), the `short` name a person types, its `title` and `glyph`
where something says them, the network it is `within`, its `cells` and
the `machines` they run on, and `counts` of its sessions as
`needs-you`, `stuck`, `moving`, `quiet` and `dormant`. A network is
heard when the latest of its cells was, or its team where its cells say
nothing. Its records change as the sessions do, so a machine come or
gone shows with the next change to a session. Ten thousand cells make
their networks within 30 MB, in time that grows with them.

The `unanswered` kind holds what waits on the mesh, keyed by the
message's id, as `mesh_read unanswered` and the hold leases judge it:
each question of the last two hours still waiting on a peer it was put
to, and each hold of the last two hours still standing. A record says
the message's `id`, `from`, `intent`, `groups`, `text`, `re` where it
has one and `since`, when it was sent. A question waits on each peer
it names by address, as `@SKIFF1`, other than its asker, and carries
them as `waiting_on`, its one live field; a peer has answered once a
message from it, under any address its session goes by, answers the
question or anything answering it, down to 20 deep, in whatever order
they come. A question asked to a group alone waits on nobody named and
is left out, as is one withdrawn (`supersedes`) or asked again by its
asker in answer to it. A hold stands until its sender answers or
withdraws it, or a message answering it carries `endsHold`, and says
the `until` it lapses at, two hours after it was sent. The node keeps
this as each message comes, never reading its 50,000 messages again,
and changes it with the messages: a session taking another address,
or a question passing two hours, shows with the next message. Ten
thousand cells' two hours of messages are judged within 100 ms.

The `said` kind holds what the person said to sessions through this
machine's node, keyed by its talk's id: each `steer` claiming the
person that has words, its newest 200. It holds the `address` it went
to, the `machine` running that session, its `text`, `at`, its `act`
where it had one, and its `talk`, and from that talk's lines, when it
was `taken_up` (delivered into the session's turn), `answered`
(replied) or `failed` (`at` and `why`). Words sent with no talk of their
own are given one, its id `said-<milliseconds>-<eight hex>`, so their
session says what became of them. Only a talk's delivered line marks
them taken up: a session's presence is written for more than its turns,
and an act such as a pause opens no talk, so it is never marked taken
up. It changes with the sessions, so a talk written while no session
changes shows with the next one that does; a talk is read again only
once its file's length or time written moves. The node keeps them in
`said.jsonl` in its own folder, readable by its account alone, and
written again with its newest 500 once it passes 1,000 lines. They are
this machine's own: no link carries them, and each machine serves only
what was said through it. The node hides none of their text, as it
hides nothing while a screen is shared; a reader showing them decides
that.

The `event` kind holds each thing a session's `events` say it did, keyed
by its session's id and its `at`: the `cell` that did it by its address,
its `session` by id, `at`, `what` it did (`commit`, `push`, `pr`,
`merge` or `stage`), `where` and `title`. An event is heard at its `at`
and never changes, and comes with its session's next presence. A page
polling the realm reads the same events as lines of `mycelium networks
-o json`; a watch of this kind is for a reader that wants each as it
comes.

A team's `glyph` is the shape every view draws it by: `square`,
`triangle`, `plus`, `hexagon`, `circle`, `down-triangle` or `star`.
Its first choice is its `orchestration.<address>` name hashed as the map
page hashes it, each UTF-16 unit added to 31 times the hash before,
wrapping at 32 bits, modulo seven in that order. The oldest team, by its
oldest session's id, chooses first, and one whose choice is taken steps
on to the next free shape. Past seven, a team takes the shape held by
the fewest teams it is drawn beside (within the same team, or within it
or holding it), then by the fewest at all.

A list naming `fields`, a list of paths of keys joined by dots such as
`presence.usage.context`, is sent only those fields of each record,
with its `key` and `kind` always; a path a record lacks is left out
rather than sent as null, and so is an object left empty. A reader of
thousands of sessions needs a few dozen of each one's hundreds of
fields: `mycelium networks` reads 32, about a quarter of a session's
bytes. Paths may not index lists, and more than 64 of them, or a path
that isn't one, is `not-an-ask`. A watch sends records whole, as
before.

A watch sends what
changed by key, with only the fields that differ, a dropped one as null; nothing
while nothing changes; and no sooner than 250 ms after its last send, so
a reader too slow for it is sent the latest rather than a backlog. A
watch asked `when` some fields move, as a shut launcher asks for a
session's `told`, sends only the records whose change moves one of
them, and those brought or taken, each with all that moved in it since
it was last sent; the rest wait for their own. Joel's 103 sessions
changed 56 times in a minute on 2026-11-11, and none of them moved a
`told`; once one did, every session's held step went with it, about
30 KB a line. Asked again under its `id` for the same kind and keys,
a watch keeps on, sending at once what it held back, and from then on
as `when` now says, rather than everything whole again.

A watch asked `deep` some fields, among those its kind's `hello` offers
`deep`, says a change to one, an object before and after, as only what
moved inside it, a dropped one as null, rather than all of it; the rest
of `deep` is taken for nothing, and what it held back comes the same
way. A session offers `presence`, most of its record, whose time moves
at every beat: of Joel's 80 sessions' changes held back over 90 s on
2026-11-15, 128 KB of 207 KB was presence that hadn't moved. Asked
again with another `deep`, a watch starts over, its records whole. A
node whose `hello` offers nothing `deep` says every change whole. An ask
it can't answer gets `{"id", "error": {"code", "message"}}`, the code
one of `unknown-ask`, `unknown-kind` or `not-an-ask`, or `refused` for an
ask understood that it couldn't do, its message saying why.

A `steer` puts what the person says, in the shape `Steer` takes, into
the inbox of the session going by `address`, on the one machine running
it, as `Steer` does: here, or over the link reaching that machine. It is
answered once that machine has it, and the connection's other asks are
answered meanwhile, so a steer to a far machine holds up no watch. A
session no machine runs, or one two do, is `refused`, saying which.
An `update` asks `machine` to update itself, as `"Update"` does, and
is answered once its updater says how it ended, the connection's other
asks answered meanwhile; one no updater or link answers is `refused`.

The node also serves files another program keeps, each as a kind of its
own, so a reader asks the node and hears each change rather than reading
the files itself. `mycelium node --serves FILE` names them, in a JSON
object of sources by kind:

```json
{
  "quest": {"file": "/full/path/quests.json", "records": "quests", "key": "id"},
  "waiting": {"folder": "/full/path/waiting", "records": "items",
              "key": "id", "by": "quest"},
  "queue": {"file": "jobs/queue.json", "within": "runtime", "whole": true},
  "talk": {"talks": "/full/path/steer"}
}
```

A source is a `file`, or a `folder` whose `.json` files each hold some,
by its full path, or for one kept where it differs from one machine to
the next, by its path `within` the login's `runtime` folder, going down
from it. `records` names the field holding them: an array of
objects, each keyed by its `key` field, or an object of them, each keyed
by its own key. A folder's file puts its `by` field in front, so
`waiting:QEST-A:login` is record `login` of the file whose `quest` is
`QEST-A`. A file that says how something stands, rather than listing
things, is held `whole` instead of naming its `records`: one record, its
id the machine's name, holding every field of the file. One whose name
ends `.jsonl` is of JSON lines, as a program appending what happens
keeps one: its record holds `lines`, each whole line in the order
written, as a talk's does. Each record's
fields pass as they are, beside the interface's
own, `heard` being when its file was last written, and none is marked
live. The node reads a source whole whenever a change reaches its
folder, as its program writes each file whole and renames it into
place; a file it removes takes its records with it, sent as `gone`. A
file or folder not there, not written yet or removed, holds nothing,
which is no trouble, and a folder not there is looked for again each
minute. A source it can't read, or one of a folder's files, is a source
`refused` in `health`, with why, while the records it could read are
still served. A kind the node holds itself, `session`, `machine`,
`message`, `team`, `network`, `unanswered`, `said` or `event`, can't be
served.

The talks of the steer inboxes are the one shape the node knows, as it
carries them itself. A source naming `talks`, the inboxes' folder and
nothing else, serves each `<ADDRESS>/replies/<id>.jsonl` as one record,
its id `<ADDRESS>/<id>`, holding its `address`, its `id` and its
`lines`: each whole line of JSON in the order written, a line still
being written left for the next read. Only each inbox's newest 20 talks
are served. The inboxes are watched to any depth, so a talk is heard as
its session writes each line, in an inbox made since.

## A Held Session

`mycelium holds add -- <program>` starts a program on a terminal that
a holder of its own keeps, so the window it started in can close and
`mycelium holds <id> attach` take it up again from any terminal on the
machine. `hold` is the same as `holds`, and the older spellings,
`hold -- <program>`, `held`, `attach <id>` and `again <id>`, still
work. The holder is its own process, in a session of its own, rather
than the node's: restarting the node's service stops everything the node
started, and a closing window hangs up on its own session only.

Each holder serves `<id>.sock` in `<runtime>/mycelium/<realm>.held`,
reachable only by its account, beside
`<id>.json`, which says what it holds: `id`, `command`, `folder`, the
`holder`'s and the program's `pid`, `since`, `parked` while it is, and
the `windows` joined to it that said which they are, the latest last. A
window leaves the record the moment its terminal leaves, and both go
when the program ends. `mycelium holds` lists every record whose
holder still runs, and `mycelium holds <id>` prints one. Each start of
the program, the first and every one after a park, has the person's
environment as their service manager has it then (systemd's user
manager's `Environment`, read with `busctl`), over the one its holder
was started with, keeping only the holder's `PWD`,
`OLDPWD`, `SHLVL`, `_` and `TERM`, and its own `PATH` first, with only
the manager's folders it lacks after it, since the shell it was started
from puts there what the manager never hears of, such as a developer's
own tools a program runs as it starts. A holder can be started hours before
its program, by a node at login or by a hold from a window whose opener
began long ago, and a variable the person's configuration gained since
would otherwise never reach it. A manager that doesn't answer within two
seconds, or a Mac, which has none that lists it, leaves the holder's
own. The program is told its id in `MYCELIUM_HELD`, and loses
the variables naming the window it started in. It is in the realm it
is held in, told in `MYCELIUM_REALM` after the login shell's profile
runs, since a profile says the machine's own realm: a session started
in another realm of a machine in two would otherwise join the wrong
mesh.

A program whose words resume a session, after `--session` or
`--session-id`, whole or within a shell's words, waits for its turn
before its first start, so a restart's sessions come back a few at a
time. Its holder says `held` at once, records it `parked` with a
`pid` of 0, and asks its realm's node `{"Turn": {"session"}}`,
holding the connection open. The node answers `"Turn"` at once for a
session no record names, which has nothing to resume, and otherwise
once fewer than 4 resumed sessions are on their first turn. A turn
ends when its session is heard idle or ended by its new process, one
that was working only once heard working again or idle for 10
seconds, or after 60 seconds.
Those waiting go in order: sessions whose record wants their person
for a `question` or an `approval`, then those that were busy, then the
rest, each in the order it asked. Every joined window says it is
waiting, and a key typed in one starts it at once, leaving the queue.
The queue is the node's memory only: a node that restarts, isn't
there, or doesn't know turns answers or hangs up, and the program
starts at once, as before.

A program that exits with 75 (sysexits' `EX_TEMPFAIL`) is parked, not
ended, so a session waiting idle can give its memory back. Its holder
keeps its socket, its terminal and every window joined, says `parked`
with a `pid` of 0, and starts it again on the same terminal when a key
is typed in a joined window, the key itself going nowhere, or when
`<id>.wake` appears beside its record. The program is told that file's
path in `MYCELIUM_WAKE`, and the holder takes it away once it has woken
the program. It starts the same command again, unless the program wrote
`<id>.again` as it parked, a JSON list of a program and its arguments,
which it is told the path of in `MYCELIUM_AGAIN`: so a session started
as a bare `pi` comes back to its own conversation. That command is the
one started, and the one the record says, from then on, and the file is
taken away once read. A command named from outside, in `<id>.next`,
wins over it, since a program names itself: a pi held on an old
conversation would otherwise write that one over a newer one named for
it while it ran. `mycelium holds <id> again -- <command>` writes that file
whole and wakes the program if it is parked. A program may also write
`<id>.steers` as it parks, whole, told its path in `MYCELIUM_STEERS`:
`{"inboxes": [<folder>, ...], "wakes": {"acts": [<act>, ...], "talk":
<bool>, "resumeWithWords": <bool>}}`, the steer inboxes it reads and
which steers wake it. While it is parked its holder watches each of those
folders, not recursively, and wakes it as a steer lands there whose `act`
is one of `acts`, a `resume` with `text` when `resumeWithWords`, or `text`
with no `act` when `talk`; it looks once as the watch begins, and the
look a second watches for an inbox made later. The steer stays for the
program to take, and the file is taken away once it has woken the
program; a holder that finds none, or can't read it, wakes it as before.
A program a node started
runs through the login shell at every start, not only its first, since
its holder has only the node's environment, where a bare `pi` isn't
found; the record says the program, not the shell. One that can't
start again stays parked and says why on its terminal, and the next
wake tries again, with any command named since. A parked program is no
work: it keeps no machine awake and holds up no handover.

A joined terminal sends frames of a tag byte, a two-byte big-endian
length and what it carries: tag 0 is bytes typed, and tag 1 is its size,
rows then columns, two bytes each, sent on joining and on each change.
Tag 2 is the window the terminal is, as JSON in a session's `window`
shape, once on joining when it can say: `mycelium holds <id> attach` in a kitty
window says its id and kitty's socket.
The holder sends back the program's output as it comes. Any number may
join at once; the program takes the latest size it was told. One joining
at the size the program already has makes it one column narrower first,
so the program draws itself again. One that falls so far behind that it
misses output is caught up the same way, given the modes again and the
program nudged to draw itself whole, and the holder says so on its log,
so a window never shows a gap. Leaving is closing the connection;
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
- **Providers:** programs beside a node that offer shapes of machine
  and make one for a start that may cost (`"Offers"` and `"Start"`,
  above), reach it and let it go, such as Aquifer's, which lives
  outside this repository; next, each says when one is lost and what it
  cost. A machine a provider made shows as a place like any other. A
  machine that can't dial out is dialled in, through its provider's
  program (`--via`, above), reaching a node that answers links on a port
  only its own machine can reach, which is next.
- **Connections of the node's own:** machines dialled by key, found on the
  network without a cloud, and encrypted always, behind `Dial` and
  `Discovery`; membership then checks the realm's enrolled keys. SSH
  and providers' programs stay the only dials until per-pair keys become
  the burden, or SSH itself makes a link miss 5 ms.
