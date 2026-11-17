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
   "steers", "releases", "updates", "runs", "carries", "shows",
   "ends"}}`, first,
   from each end. Each end's
   membership admits the other or refuses it, and nothing else crosses
   until it has. `facts` says the end hears and says what a machine is; an end
   older than it says nothing of machines and is sent nothing. `starts`
   says it carries starts, and `steers` steers; an end older than either
   is asked none of it. `releases` says the end's way to the other
   end's machine can release that machine, as a provider's program can;
   only such an end is told the machine has handed over. `updates` says
   it carries update asks, `runs` runs and `carries` carry asks; an end
   older than any of them is asked none of it. `shows` says it tells of
   the carries its node ordered and keeps those the other end tells of;
   an end older than it is told none. `ends` says it carries an end of a
   session held there; an end older than it is asked none.
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
15. `{"Run": {"ask", "run"}}`: run a command on the other end's machine,
    as a `"Run"` request says it. The other end takes it only if its
    membership proved which machine asked, as for an update, and hands
    it to its own node's socket pinned to its machine, its `from` the
    asking machine and, where it says no `by`, as a strictly fenced
    agent's; otherwise it refuses. Only the end that dialled asks a
    run: the dialled end refuses one its node asks, since its link runs
    outside the node. Either way it answers `{"Ran": {"ask", "ran"}}` a line for
    each `Ran` the run says, the last its `Exited` or `Failed`. It sends
    at most eight pieces of output ahead of `{"Taken": {"ask"}}`, which
    the asking end sends as its asker takes each, so a run writing
    faster than its asker reads waits rather than filling the link.
    `{"Unrun": {"ask"}}` says the asker let go, and the run is stopped;
    a link that drops stops every run it carried.
16. `{"Carry": {"ask", "quest", "to", "checked"}}`: carry `quest`'s
    sessions off the other end's machine to machine `to`, for the
    person, answered by `Started` with `Carried`. `checked` says this
    end's node saw the person's own login ask, and is left out when
    false. The other end takes it only if its membership proved which
    machine asked and `checked` is true, then runs the carry's steps
    against its own node's socket, as `mycelium carry` does; otherwise
    it refuses, saying why. No time limits the answer, since a carry
    waits minutes for its sessions to end.
17. `{"Carries": {"carries": [carry]}}`: the carries the sending end's
    node ordered or settled within the last day, newest first, after
    its Machine and again whenever they change, so the person sees each
    from every machine (see A Carry an Orchestrator Orders). Each holds
    `at`, `by`, `session`, `address`, `quest`, `from`, `to` and `why` as
    kept, `settle` once it settled, `left` saying whether its session's
    done file was written here since, and `refused` saying why once it
    wrote a refusal since. At
    most 200 cross at once, coming to at most 256 KiB as JSON; a sender
    with more sends its newest that fit, and an end told more refuses
    them and keeps what it held. They cross only where both hellos say
    `shows` and neither says `releases`, and never to or from a worker,
    since quest ids and reasons aren't work. So a lent machine's link
    and a worker's carry none either way, and a frame sent otherwise is
    refused. Each end
    sends only its own node's: one it was told it never passes on, and
    none is taken as an order.
18. `{"End": {"ask", "held", "by"}}`: end the session held on the
    other end's machine as `held` for good, answered by `Started` with
    `Ended`. `by` is who this end's node judged asks: `"Person"` for the
    person's own login, or `{"Session": address}` for the session asking.
    The other end takes it only if its membership proved which machine
    asked, as for an update, and `by` is the person, since a session's
    orchestration is proved only where both run; otherwise it refuses,
    saying why. It ends the hold itself, as `"End"` on the socket does.
    No time limits the answer, since the session has ten seconds to
    save what it holds and what is killed past them two more to go.

Only one link joins two machines, and whichever dialled first holds it,
so each end carries what its own node asks of the other machine: starts,
steers, updates, carries and ends, and runs from the dialling end only. The
dialling end runs in its node and takes them there. The dialled end runs
in a process SSH started, outside its node, so it holds a `Carry`
connection to its node's socket (see The Socket), asked again every
five seconds while the node can't be reached. Its node
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
  SSH key is. A machine reached so is a worker: it is asked and asks
  nothing back. Its end's starts, steers and updates are refused, its
  follows are ignored, and a presence of its naming another machine
  isn't kept, nor a reply to an ask it wasn't sent, nor what it tells
  of carries. It is said and
  served only the quests it was given, each by a `mycelium share --to`
  naming it, which gives that quest alone and none above it, and takes
  back only those and
  the quests it began, which this machine holds none of; it can end
  no other. A message of its isn't kept when it names as its sender a
  session another machine holds, or when this machine already holds
  one by that id, while its own sessions' messages and presence cross
  as any machine's do. The name it first says hello as is the one it
  is held to: a redial of the same place saying another is refused.
  Either way, the far `mycelium link` looks each minute at
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
`node_modules/` stay home, and below a quest's record or scratch folder
the node follows no link it finds as it reads or writes a file, whether
the file is one or a folder it is in: such a file reads as absent, and a
write to one is refused. The node checks each path as it comes to it, so
a link a process on this machine makes between that check and the read
or write isn't caught. A folder once named stays shared. The
scratch's `handover/` crosses whenever it is there as the quest is
shared, named or not, under the same limits, since a carry or a spawn
leaves there what the other machine needs to start. Sharing a quest
shares each quest above it too, up its README's `parent` while each is
on this machine, so a session on the other machine can tell what
orchestrates it. One shared only so carries its `README.md` alone, none
of its attachments, journey, other documents or scratch, and no
`Shares` names its scratch or clones; it carries its whole record once
either end shares it on its own. Nothing above is carried to a worker,
and an end is taken for one until its `Machine` frame declares it is
not, so an end that takes no facts, saying no `Machine` frame, is sent
none, and the node withholding logs so unless it links that end as a
worker. A hold lifted takes up what the other end said of its scratch
while withheld. One shared only so stops being shared, at both ends,
once no quest either end shares names it above and the other end
doesn't share it on its own; one shared on its own as well stays. Only
a quest shared in its own right keeps a quest above it, so parents
naming each other can't keep each other. A parent shared before ends
said `above` carries no mark, so it never stops.

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

- `{"Shares": {"quests", "scratch", "clones", "ended", "above"}}`: every
  quest it shares, the scratch folders named of each, by quest, the
  clones in them, each `{"remote", "branch"}` by its path, the quests
  whose README says `concluded` or `retired`, and those it shares only
  as quests above others, which the other end shares so too and doesn't
  take up once none is under them. The other end starts sharing
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
2. `push`: before any is told, the node takes away each
   `<session>.cell.json` in the quest's scratch, under
   `handover/<this machine's short hostname>/` (up to its first dot),
   of a session it won't carry whose `<session>.done.json` is gone or
   started: an earlier carry's, refused or finished. One beside a done
   file not yet started, as a failed carry leaves it, stays, so a carry
   taken again keeps it; a cell beside such a done file is the newest
   carry's to write one for that session, though not always the one
   that wrote the done file. Anything there under a cell's name that
   is a folder is passed over, though one at the name of a session it
   will carry stops the carry, saying so. Then it writes `<session>.cell.json`
   there for each it will carry, replacing whole any an earlier carry
   put there: `{"session", "cell", "ties", "talk", "formed"}`, where `cell` is
   the session's address, uppercase; `ties` every `tie` record it
   holds with the cell at an end, whole and unfaded; and `talk` its
   `tie:<cell>` record, or null; and `formed` each `formed` record,
   whole, of a tree network the trees its presence holds make, so the
   node it arrives on can keep when that formed: at most 64, earliest
   `at` first, those at one time by name, and one whose `at` isn't a
   whole number last, so one left out forms afresh there. The file
   lies where sessions can write, so that node reads it as a claim,
   keeping at most 64 formed records itself, each only for a network
   the arriving session's own trees make. Nothing else of the cell
   travels, and an address holding `~` brings no ties. The ties are as
   they stood before any session was told, so talk in a session's last
   turn stays behind. Formed networks are asked for only where a
   session's trees make one.
   Where a cell can't be taken away, or the ties or formed networks
   can't be said or written, nothing is carried. A session that refuses
   writes no done file, and the node takes its cell away then, unless
   an earlier attempt's done file not yet started waits beside it; the
   others' cells stay, since they may still be carrying. One that
   doesn't carry in time, or refuses after the node stopped looking,
   keeps its cell until the next carry from here. A session refusing
   an earlier carry's steer still waiting for it is taken as refusing
   this one, and may yet carry without its cell. A handover shared
   already may take such a cell across first, so the reading node
   reads a cell only beside its done file. A cell goes only where its
   done file goes, in the same handover. The handover is the sessions'
   to write too, so a cell is the node's word only as far as the node
   reading it checks its `cell` against the arriving session's
   address as its own records say it, keeps only ties with that cell
   at an end and a `talk` keyed `tie:<cell>`, and takes their counts
   as the sessions' word. Then each is steered
   `{"act": "carry", "to": <machine>}`; the harness puts every tree it
   holds and the session's file into the quest's scratch, in that
   handover, writes `<session>.done.json` there saying what it put (at
   least `session`; the harness adds its folder and trees, within its
   home), and ends.
   Once each has, `pi-carry QEST-... --to <machine>` carries what no
   session held. A cell is read only with the done file it sits beside,
   as that session starts there. A carry taken again here takes away
   the cells of sessions an earlier attempt started, so the node there
   should read a cell as soon as it has the done file beside it.
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
   stands, never made. Pi starts with `PI_SANDBOX` set to what
   `pi-carry fence` prints from the done file, so it arrives in the
   fence it ran in; it doesn't start where that fails, and starts as
   the node runs where an older done file names none. Its first prompt
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
- The fence it names is never looser than its process shows. The
  node reads the proved owner's process: `PI_SANDBOX` in the
  environment it began with (`strict` or `fence`), and, for a pi that
  owns its session alone, a `bwrap` running anywhere below it, which
  shows it fenced. A presence naming a looser fence than that is
  refused, as `HARBOR says it runs unfenced, but its process shows it
  fenced, so where it may land can't be told`; one naming a stricter
  stands, since the strict fence leaves nothing lasting to show. A
  process that can't be read, one that ends while it is read, or an
  owner that can't be proved, is refused too, and so is a session a
  pi host runs, since the host's environment says nothing of each
  session's fence.
- A fence missing from what a process shows proves nothing: a
  `bwrap` lives only while a command it fences runs, and a session
  may switch its fence on after its pi began. So the node looks at
  its sessions' processes as their presences change, at most every
  two seconds, and keeps the strictest fence each was ever seen in as
  `fences.json` beside `carries.jsonl`, which no session lowers. A
  carry is judged by the strictest of what the presence names, what
  the process shows now and what was seen before, so a session once
  seen strict that later names a looser fence is refused. Where that
  file can't be read or kept, every ordered carry is refused. A
  session here seen fenced says so in its record as `fence_shown`.

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

`mycelium carry QEST-... --to <machine> --why <reason>`, run from the
orchestrator's own session, takes the steps above with the steer
`ordered-carry` in place of `carry`. The node answers each such steer
`{"Steered": {"machine", "by"}}`, `by` the orchestrator it proved, and
the carry is refused if `by` is missing, since a session it started
couldn't arrive as anyone's, or if it differs between sessions, rather
than stamping the last. A blank reason is refused, by the command and
by the node. With no session of the quest here, it is refused too:
only a session's steer proves who orchestrates it, so only the person
carries what the quest left. A session that won't go, as one holding a
lease, mid-push or mid-rebase, or with background work running, writes
`<session>.refused.json` in the handover folder, `{"session", "why",
"at"}` and nothing more, with `at` in RFC 3339 and `session` the one
the file is named for, and the carry fails at once with `<address>
refused its carry: <why>`, never waiting it out. The harness writes it
whole, beside the folder's files and renamed into place. Only a file
changed since this carry began, saying a time since, counts; one
changed before is passed over unread, and one that can't be read yet
is read again at the next look. One naming another session than its
file stops the carry. Each session it carried starts with `pi-carry
arrive ... --by=<address> --why=<reason>` after the person's arrival
words, each value joined to its option so a reason opening with a dash
is still the reason, `by` taken from the node's answer, never from the
command line or the session's done file, so it arrives saying who
carried it and why, never as the person.

The node that kept a carry serves it in the daemon interface as a
`carry` record for a day after it was kept, or after it settled, its
id the machine whose node kept it, the carried session's and `at`, as
`kestrel:s-harbour:2026-11-16T08:00:00Z`, holding `machine`, and `at`,
`by`, `session`, `address`, `quest`, `from`, `to` and, where the line
has one, `why`, as kept,
`settle` once it settled, and `state`, judged from what the node sees
itself, never from what a session or its orchestrator says: `refused`,
with `refused` saying why, once the session wrote a refusal
since; `arrived` once it runs on `to`; `back` once it runs here again
after its done file was written since; `carrying` once it is gone from
here, by its presence ending or running somewhere else; and `ordered`
until then. Its `state` and `refused` are live. The chain and asker
stay in the file, which proves them. Each node serves the carries it
kept, and those each linked machine's node told of in a `Carries`
frame while a link carried it within the last day. Of those, `refused`
and `left` are as that node told them, since only its machine's
handover shows them, and where the session runs is as this node sees
it, so an arrival shows at once and a refusal within a minute. Once no
link has carried a machine for a day, what it told is let go.

A carried session leaves its worktrees on the machine it left, so that
machine's node lets them go once the carry is done with them: once a
carry it kept from here stands `arrived` and was kept more than a
day ago, it runs `pi-carry settle <quest> --session <session>`,
off its loop and in its own environment, saying in its log when that
takes over a minute and waiting on it, never ending one part way
itself. A service manager stopping the node ends it too, as it ends
what the node started; a node stopped alone leaves it running. A
carry whose session this node saw arrive stands `arrived` for
settling while its session runs nowhere, as when it ended there or
is on its way somewhere else, until it is seen back here. A node
started afresh has seen none arrive, but takes one with a line in
`settles.jsonl` as arrived until it sees its session back here, since
it stood `arrived` when asked. It looks as presences
change and each minute, at the newest carry of each session kept
within the week; never at one standing `refused`, `ordered`,
`carrying` or `back`. Settling lets go of each tree holding
nothing the carry didn't take, and otherwise keeps every one, saying
why. `pi-carry` itself settles nothing of a session its own session
record says runs here again, and keeps each tree the work broker's
record says a live session holds or a running process works in. It
reads each of those once, before letting any tree go, taking a record
or a process it can't read as nothing there. So a tree entered or
edited between those reads and the letting go is let go all the same,
its edits with it, whether the carried session came back or anything
else did. The node can't stop a settling it sees the session back
during, and keeps `settled` where `pi-carry` answered so. What it
came to is kept as a line of `settles.jsonl` beside `carries.jsonl`:
`carry`, the carry's id without its machine, `at`, and `did`, which
is `settled` with `let`, how many trees it let go, or `kept` with
`why`. Where `pi-carry` answered and settled nothing, by
its `failed` line, an end saying nothing it settled, or any other
exit, `why` says which, with `again`, `true`. Where its quest or
session begins with `-` and would be read as a flag, or it names
none, `pi-carry` isn't asked and the line has no `again`. Each carry's
newest line stands, even for a node started afresh: one without
`again` for good, so its trees stay, and one with `again` for a day,
when `pi-carry` is asked again while the carry is within the week,
since a shell or an editor standing in a tree may have left it since;
past the week nobody asks. Once a `kept` line is kept, the node tells
the carried session, at the address its carry names, wherever it runs
now: a finding from `mycelium@<machine>` saying its trees stay, why,
as `pi-carry` put it, and whether `pi-carry` is asked again, so the
session takes what's in them or lets them go. Once a `settled` line
follows a `kept` one, it is told they're let go since. It isn't told
where its line says what the one before it said, where the trees were
let go at first, nor where its session came back here, since the trees
are its own again. It is told at most once: a send that fails, or a
node stopped between keeping the line and sending, isn't tried again.
As the node keeps a line, it lets go of each kept more than a
fortnight before, since its carry was ordered before then and is no
longer looked at, and of any torn by a write cut short. One whose
`at` can't be read, or lies more than a fortnight ahead, stays, since
it wouldn't fade by itself, and each time a line is kept the node
names it on standard error, by its carry or else its line's number,
until it is removed by hand. The fortnight is twice the week, so a
clock stepped back by less than a week brings no carry back without
its line. A node keeps one line at a time under a lock on
`settles.jsonl.lock`, which holds too between two nodes given one
state folder, writes the file aside and moves it into place: a reader
sees the old lines or the new. One with
`again` whose session this node sees back here gets a line without
it, `why` saying so, at each look until that is kept, as does an
answer kept once it was seen back, since only a node that saw it come
back knows; a session back and ended again while the node was stopped
isn't seen. Trees let go don't come back if the node or `pi-carry` is
rolled back, though each one's branch still names its commits. What
asked nothing keeps no line: a
`pi-carry` that didn't run, or one that can't settle (its exit 2, as
one from before settling gives). That, and an answer lost, as when a
signal ends the settling or its line can't be kept, is asked again
only once the node starts afresh, which settling allows. Where the
file can't be read, nothing is settled. The carry's record then holds
`settle`, `{"did", "let" or "why", "again", "at"}`, its newest line,
and is shown for a day after it settled as well as a day after it was
kept, on every machine its `Carries` reach.

A carry the person's own login asks, with `mycelium carry` or the
one-act `carry` ask, is kept in `carries.jsonl` too, as each `carry`
steer reaches a session here, and only where the asker was the
person's own, as this node judged it or, for a carry a link brought,
as the linked node said it checked: `by` is `person`, as the node
judged it, since a steer giving `by` is refused. It holds no `why`,
`asker`, `chain` or `source`, since the person gives no reason and is
asked by nobody. Its quest is the one whose record the node serves
holds the session running here, never the quest its presence claims.
A carry naming no machine but this one, or of a session no quest's
record holds running here, is carried with no line, since nothing
settles a carry without its quest, and the node's log says why, as
it does for a carry whose asker's process it can't tell as the
person's own login. One that can't be kept isn't carried, so its line
is kept before the steer is delivered: one delivered with no line
would leave its trees to nobody. A steer that then fails leaves its
line, which stands `ordered` while its session runs here, so isn't
settled. A kept one is served, told to linked machines and settled as
one an orchestrator asked is. A one-act carry
asked of a node its quest doesn't run on is kept where it runs: that
node takes the asker as the person's on the word of the node that
asked it over the link, as it takes any steer a link carries, then
steers its own sessions through its own socket, where it is the
person's own login. A steer a link carries to an inbox keeps no line,
since that end knows no sessions or quests.

### A Machine Easing Itself

A node run with `--eases` looks each minute at what its machine would
do to ease itself, judging only by what it reads, and does none of it.
It reads the realm's balance as `mycelium balance --follow` does and
holds what is seen in most of the last ten readings. Once its machine
has been held over for ten minutes while another has room, it would
put its sessions that wait to sleep where they are. If the readings
five minutes on still say over, it would carry one session to a peer
with room: the smallest log first, placed as any work is and onto
ground as firm. A session in a turn, on the person's screen, holding a
lease or a heavy run, or whose quest runs elsewhere too stays. So does
one it can't tell any of these of. What a session is part way through
is told only once its presence says `"leases": {"background": true}`,
its harness keeping a lease for each background run. After a carry it
would carry none for half an hour. A session moved by anyone stays put
for a day, by the carries kept here, and nothing goes to a machine a
session came from within the hour.

Each choice to sleep or carry, and each change in why it does nothing,
is a JSON line in the state folder's `eases`, a file each day of at
most 8 MiB, the folder its owner's alone. Each line holds `at`, `would:
true`, `choice` (`nothing` with `why`, `sleep` with `sessions`, or
`carry` with `session`, `to` and `why`) and the `reading` it was chosen
by. A carry's line adds `holds`, what the session's own process holds
where it has one: `memory` resident in bytes, and `cpu` in cores over
the last ten minutes. What it would have done counts in its memory
alone, so a session it would have put to sleep counts as asleep, and
one it would have carried as gone. Nothing reaches the record of
carries.

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
| `"Builders"` | `{"Builders": [builder]}`, the builders it sends Nix builds to |
| `"Offers"` | `{"Offers": {"offers", "unanswered"}}`, asked of each provider now |
| `{"Place": work}` | `{"Placement": placement}`, where the work goes |
| `{"PlaceToStart": work}` | `{"Placement": placement}`, as `"Place"`, counted here as a start |
| `{"Start": {"work", "folder", "command", "may_cost"}}` | `{"Started": {"machine", "held", "folder", "attach"}}` |
| `{"Sprout": {"words", "folder", "from", "on", "cwd"}}` | `{"Started": {"machine", "held", "folder", "attach"}}`, as for `"Start"` |
| `{"Steer": {"address", "steer"}}` | `{"Steered": {"machine", "by"}}`, where it was put, and who ordered a carry |
| `{"Orchestrates": {"heads", "quests"}}` | `"Orchestrated"`, recorded (A Carry an Orchestrator Orders) |
| `{"Free": {"quest"}}` | `"Orchestrated"`, freed, from the person's own login |
| `{"Update": {"machine"}}` | `{"Updated": ending}`, as its updater said it |
| `{"End": {"held"}}` | `{"Ended": {"machine", "held", "ending"}}`, `ending` `gently` or `killed` (A Held Session) |
| `{"Run": {"work", "folder", "command", "by", "from"}}` | `{"Running": {"machine"}}`, then a `Ran` a line |
| `{"Messages": {"groups", "limit"}}` | `{"Messages": [message]}`, oldest first |
| `{"Send": {"from", "groups", "intent", "text", "re"}}` | `{"Sent": message}` |
| `{"Send": {"from", "groups", "intent", "text", "re", "realm"}}` | `{"Sent": message}`, or refused |
| `"Flows"` | `{"Flows": {"to"}}`, the realms a `Send` may name |
| `{"Cross": {"message", "from_realm"}}` | `{"Sent": message}`, or refused |
| `{"Watch": {"groups", "recent"}}` | `"Watching"`, then events; no `groups`, no messages |
| `"Present"` | `"Present"`, then held open while its process runs |
| `{"Carry": {"machine"}}` | `"Carrying"`, then what it asks of `machine` |
| `"Health"` | `{"Health": health}`, then another each time it moves |
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
{"address", "steer", "checked"}}` or `"Update"`, and the link writes
back `{"ask", "reply"}` with the other end's reply. A run isn't carried:
it goes only where this machine dialled. It lasts until either
hangs up, or until another link reaching the machine takes its place;
an ask still waiting then hears that the link dropped.

A `Health` connection is how the end of a link another machine dialled
says this machine's own health in its facts. The node answers with the
health it keeps, or `null` while it is unknown, then again each time it
moves, until the client hangs up or sends anything more. A node too old
to know the request refuses it, and the link says no health.

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
link of this node's reaches, and none while no link does. Over SSH,
those are `mycelium --realm REALM attach --on MACHINE ID -- WORDS`,
`WORDS` being SSH's to that machine's own: run here, it runs `WORDS`
and runs them again each time SSH ends 255, as it does when its link
drops, putting back the terminal modes the program there left set
(the kitty keyboard flags, the alternate screen, a hidden cursor,
mouse and focus reports and bracketed paste) when its standard error
is a terminal, saying `Reconnecting to MACHINE.` and waiting 1
second, then twice as long each time up to 30, back to 1 after a join
that held 10 seconds. Before each try it asks this node's `Sessions`, and once they
list the session `ended` and not parked, it says `TITLE has ended.`
and exits 0; one they don't list, or a node that doesn't answer, is
tried again. It exits with any other code the join ends with, and 130
on Ctrl+C. A provider's program says no code for a dropped way, so its
words are served as they are and run once. A session
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
(`model`, `cores`, and `busy`, the share of its cores' time spent busy
across the last five seconds or more, 0 to 1, absent from a node too old
to count it, which on a Mac is one older than its other facts, and from
any node in its first five seconds or while its system won't say),
`memory`
and `disk` in bytes (the disk holding its
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

Its `health` is how the machine stands as its own health daemon says it,
where its node is given that daemon's socket (`--health-socket`): the
whole machine's word, `health`, and `parts`, each part's word by its
name. A word is `good`, `watch`, `bad` or `unread`, and any other, a
node's own or one a link carries, is read as `unread`. A part's name is
at most 32 of `a` to `z`, `0` to `9` and `-`, and there are at most 16
parts; any other part is left out. The node asks the daemon in the
daemon interface, `hello`, then a `watch` of kind `summary` and one of
kind `part`, and keeps from each record only its `health`, and a part's
`part`: never what a part says, nor when it was read, so the health
moves only when a word does, and holds under a kilobyte. A key the
daemon lets go (`summary`, or `part:NAME`) is let go too. The health is
absent, not `null`, while no daemon is given or answers, before one has
said the whole, and from a node too old to say, and absent reads as
unknown. A daemon that goes away having said the whole is asked again
after two seconds, and one that never said it after twice as long each
time, up to a minute. A line past 64 KiB ends the watch, as a daemon
going away does. A word moving is said at once: the node keeps it in
its last measure, and each link says its machine again, a link another
machine dialled hearing it over a `Health` connection to its node.

Its `kit` is its realm's kit as its node was given it (`--kit`): a
folder whose `bin` holds the tools every machine in the realm is to
have, such as a Nix profile built for the realm, which each session and
run the node starts finds first on its path. It carries `path`, where
the folder is with any link to it resolved, which names its version
where it is in the Nix store, and `tools`, the name of each entry in
its `bin`, leaving out any beginning with a dot, in an order nothing
should lean on. Where the node refuses the kit it carries `refused`,
why, with `path` the folder as it was given and `tools` empty, so a
node too old to read why still finds no tool there. It is absent where
the node is given no kit or is too old to say, and absent reads as
unknown. Both the node a machine dialled and the links it keeps say it.

A kit is a whole path holding no colon, which the node refuses
otherwise. Its tools come first for every session and run, an agent's
fenced ones too, so it belongs where no agent can write, such as the
Nix store. The node checks it once, as it starts: where an account but
root could change the way to it, any link on that way among it, its
`bin` or anything in that, hidden entries too, or where a tool leads;
where it resolves to a folder holding a colon; or where it isn't there
or its `bin` can't be listed, the node refuses it, still starting but
putting the kit on the path of nothing it starts, and says why at its
start and in its machine's `kit`. A tool an agent could replace would
otherwise run as every session and run that asks for it, other
machines' among them. A kit it carries goes on each path as the folder
it resolved to when checked, so a link repointed since changes nothing
until the node starts again; a kit the configuration moves is best
named by its store path, so moving it restarts the node. The check
reaches only what the node starts from then on: a session already held
keeps the path it was started with, a kit since refused among it,
until it ends, and one held by an older node keeps the words it was
started with.

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
queue too old to say. Its `saturated` is whether its own node judges
its processors saturated: from processor pressure 85 or more until it
falls under 75, a band about the line of 80, so a machine whose
pressure swings about the line doesn't flip with each reading. A link
the node keeps says the node's one verdict, and one another machine
dialled says it as the node answers; a dialled link whose node doesn't
answer within five seconds measures for itself and says none, so it is
judged by the line. A node with no room to read, or told its host's
verdict, forgets its own, and one restarted starts with none held. It
is left out of a room a node too old to judge says, and placement then
judges the pressure by the line of 80 alone. The room is `null` where
no harness keeps a queue, or where the queue's record is of another
shape or unwritten for two minutes. A machine keeps no reserve of
mycelium's: its own slices keep its person's share.

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
each door by the name the machine's configuration gives it, and with
`runs`, the words that start a command as that fence runs its
sessions, such as its launcher and the fence's rules. A fence
missing a door, or of a kind this build doesn't know, is read as no
fence, spoiling nothing else declared; a carry an orchestrator orders
lands only where it says one (A Carry an Orchestrator Orders). Only
the machine's own built configuration writes it, as
`~/.config/mycelium/declared.json` (or under `XDG_CONFIG_HOME`), and
the machine enforces its own taints. A node started with
`--declared <file>` reads that file instead, for everything it and the
links in its process say and run, and never the account's own, which
anything running as the account could rewrite. An end of a link another
machine dialled runs apart from the node, so it asks the node for
`Machine` and says that; while no node answers within 5 seconds, it
says only what it measures, with nothing declared. It is `null` where
nothing is declared, or where the file isn't in this shape, which the
node logs.

Its `reached` says whether each probed network answered, by name. The
node tries each probe every 30 seconds, giving each 3 to answer (a
command still running then is killed), and writes the round to
`reached.json` beside its sockets, where each end of a link on the
machine reads it; a record older than 90 seconds says nothing, so a
network is unknown rather than stale while no node probes.

`"Machines"` gives each other machine as its own end of a link last
said it, about once a minute.

`"Builders"` gives the builders this machine sends Nix builds to, which
are no machines of the realm: no session runs on one, and none is in
`"Machines"`, a placement or an offer. The node reads them from Nix
machines files, given as `--builders NAME=PATH` for builders it may
dial and `--lent-builders NAME=PATH` for lent ones it never dials, each
name saying what the file is, such as `standing` or `lent`. Each file is
read whole when its folder changes and once a minute, and one not there
holds nothing; one it can't read is a source `builders NAME` refused in
`health`. A builder is the host of a line's `ssh://` or `ssh-ng://` uri,
in lower case with a last dot and a trailing `.local` dropped, as its
`name`, or `address-` and a number for a host given as an address, which
is never served; a builder several files name is one, its facts from
the first file given that names it, dialled files before lent ones.
One any lent file names is lent, and stays so while the node runs. It
says its `systems`, `jobs`, `speed`, `supported` and `mandatory`
features, the `lines` naming it, whether it is `lent`, its `state`, why
where it says (`said`) and `heard_at`.
No key path, host key or uri query is ever kept or served.

A builder it may dial is heard at most every five minutes: `ssh -G`
resolves its host and port without the network, and it is `answering`
when that port sends an `SSH-` greeting within 3 seconds, never a login
or a build, and otherwise `not answering`. One reached through a jump
host or a proxy command is `unknown` and never dialled, so nothing is
woken, and the jump host isn't named. A builder dialled isn't dialled
again within five minutes, whatever its files say meanwhile. A lent
builder costs while it runs and idles off, so is never dialled: its
lender's health file says how it stands, at the path `--lent-health`
gives with `{host}` for its host, such as `/run/{host}/health.json`, a
JSON object of `at` (seconds since the epoch) and `state`. It is
`running` while that file is under two minutes old and its state isn't
`none`, `not running` otherwise, and `unknown` with no file to read;
neither the file's path nor why it couldn't be read is served. A lent
builder another file names too goes by its lent line's host. A builder
not yet heard is `unheard`.

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
`{"system": "linux"}`, `{"arch": "x86_64"}`, `{"reach": network}`,
`{"accelerator": "gpu"}` or `{"tool": name}`, the earliest preference
most wanted (a machine reaches a network it declares in `reach`, or one
it probes while its probe last answered, and lacks one whose probe
didn't; it has a tool its `kit` lists, lacks one its kit doesn't, and
hasn't said one where it says no kit, nor has a machine not yet made); the
keys of the taints it `tolerates`; whether it `may_wait` for a machine
out of reach; and how long it `lasts`, as an ISO 8601 duration such as
`PT2H` (`2h` reads too). A machine carries the taints its configuration
declares, and while they last, `on-battery`, `memory-pressure` (past
its queue's memory limit), `cpu-pressure` (past its queue's processor
limit), `cpu-saturated` (its `room` `saturated`, or at processor
pressure 80 or more where its room doesn't say)
and `disk-pressure` (its `disk_band` past
`healthy`), each `prefer_not`, and `low-battery` (on its battery below
20%, when it is let sleep) and `disk-full` (its `disk_band`
`critically_full`, where what it starts would fail to write), each
`never`. Work may tolerate `disk-full` by its key, as a cleanup run on
a full machine would; tolerating `disk-pressure` doesn't lift it. The
person's work is held to it as an agent's is, so where every machine's
disk is critically full a start goes to a machine made for it, or is
refused with each disk named, rather than starting where it couldn't
write. A machine qualifies when
it is in reach, leaves after the work ends (now, for work that doesn't
say how long it lasts), is what each need asks (a fact it never said
meets none), and carries no `never` taint the work doesn't tolerate.
Those rank in four tiers, each after the one before: heard and neither
over nor saturated; heard and over; heard and saturated; then unheard.
Heard, its `room` was said within three minutes either way (three of
the minutes a link carries it in); an unheard room earns no processor
or memory headroom. Over, it carries a `memory-pressure` or
`cpu-pressure` the work doesn't tolerate. Saturated, it carries a
`cpu-saturated` the work doesn't tolerate, so work starts where it runs
at once, and still goes to the roomiest where every machine is
saturated, never refused; an unheard room is never saturated, nor
passed for it. Within a tier, the most preferences met goes first,
then the most headroom, then by name; of the first and each as good but
for headroom within 0.05 of it (`NEAR_BEST`), one is chosen at random,
so asks alike spread rather than herd. Headroom is one sum, its weights
named in `src/placement.rs` to be tuned:

```text
headroom = 1.0 * (1 - processor)               CPU_WEIGHT
         + 1.0 * memory_available / memory     MEMORY_WEIGHT
         + 0.5 * disk_free / disk              DISK_WEIGHT
         - 0.5 * starts / cores                SESSION_WEIGHT
         - 0.1 * sessions / cores              RUNNING_WEIGHT
         - 0.25 * a disk filling               FILLING_WEIGHT
         - 1.0 * each other prefer_not taint   RATHER_NOT_WEIGHT
```

Its `processor` is the more of its `cpu`'s `busy` and its cpu pressure
over 100, or pressure alone where it says no `busy`: how busy its cores
are tells machines apart that wait on none, and pressure still counts
where cores are shared out short. Over still reads from pressure alone.
A share it never said counts as none, and `cores` is its declared
`cores`, else its `cpu`'s, else one. Its `sessions` are those busy
there, since an idle one's memory is already gone from
`memory_available`, and its `starts` those this node placed there by
`"Start"`, or by a `"PlaceToStart"` answered with this machine, that its
room doesn't yet show: each counts until the machine says its room
thirty seconds or more after it, since a room said sooner can't yet
show a session begun, or for five minutes at most, kept in memory only,
so a burst of starts spreads across machines alike rather than piling
on whichever led. A `prefer_not` taint such as `disk-pressure` is only
a weight, so a machine idle beside one busy may take work despite it;
a disk `filling` costs a quarter of one `nearly-full` or
`critically-full`, its guard clearing what has waited a while. The
answer is `{"placed": {"machine", "passed"}}`, `{"pending": {"misses"}}` when
only a machine out of reach would do and the work may wait, or
`{"refused": {"misses"}}`; each miss names a `machine` and `because`:
`{"lacks": need}`, `{"unknown": need}`, `{"refuses": taint}`,
`{"rather_not": taint}`, `"ranked"`, `"unreachable"`,
`{"leaves_first": time}`, `"unheard"` (its room unknown or said too
long ago) or, for an offer, `{"lives_only": duration}`.
Placement only
chooses: a machine says whether work starts there when asked, by its
own queue and taints, so nothing fits work against its room, which only
ranks it.

`"PlaceToStart"` asks as `"Place"` does, for an asker that starts the
work itself when the answer is this machine, as a pi session's spawn
does, and through `"Start"` when it is another. An answer naming this
machine counts as a start placed here; one naming another counts
nothing, since the `"Start"` that follows counts it there. So a burst of
spawns spreads as a burst of starts does, rather than each being told
the machine the last was. A node older than this answers it as a
request it doesn't know, and an asker then asks `"Place"`.

Each placement, by `"Place"`, `"PlaceToStart"` or `"Start"`, is kept
as a JSON line in `placements/<date>.jsonl` in mycelium's folder, a file
for each day of the machine's calendar, so a monitor can judge how well
work was placed and a replay can place it again under other weights:
`at`, `start` (whether it was a start, or an ask to start answered
here), the `work`, every machine's facts as placing read them
(`machines`), each machine's `loads` (`sessions` and `placed` starts
beyond what its room says), and the `decision`: the `placement`
answered and, for each machine, how it was `judged` (`machine`,
`qualifies`, `over`, `unheard`, how many preferences it met as
`preferred`, its `headroom`, and its `chance` of being the one chosen,
one in as many as were near the best and none for the rest), and the
`weights` that weighed them (`cpu`, `memory`, `disk`, `session`,
`running`, `rather_not`, `filling` and `near_best`; a record without
them was weighed by the starting ones of before `running` was said,
and one without `running` weighed each session running as a start, by
`session`). A placement
the node can't keep is said on its error output and answered all the
same. When a day's file is
first written, the files of days more than 14 before it are let go, so
the log holds about two weeks.
A processor reading that isn't a number counts as none, so every
`headroom` is a number.

`"Start"` places `work` as `"Place"` does and starts `command` held
there through the person's login shell (`$SHELL -lic`), so it has their
environment rather than the node's, and the realm's kit's tools first on
its path where its node is given a kit (`--kit`) and carries it, having
found no agent could change it, as a machine's `kit` says above: put
there after the profile runs, since a profile, or a Mac's `path_helper`,
sets the path
afresh, and a login shell that isn't interactive may never read the
file adding the person's own. It begins in `folder`: a full path
on that machine, or a path within its home, such as `src/core.nix`,
since each machine's home is its own; `mycelium start --folder` says a
folder under home that way. It begins in its home when `folder` is
none or names no folder there. The answer is the `machine`, the `held`
id, the `folder` it began in, and `attach`, the words that join it from
a terminal on the asking machine, to be run as given. A start that
isn't placed, or doesn't start, is an error saying why, such as
`nowhere: skiff isn't kestrel`. An agent's start is held to its own
ground as its run is (`"Run"`): every machine looser than this one is
missed as `looser-ground`, `never`, naming the machine and the fence
it lacks, and a start naming one is told only that, as `this asks as a
strictly fenced agent, so it starts nothing on looser ground: ...`. A
machine made for it is held to the same once it links, and let go
where it is looser. The person's starts aren't. A start relayed from
another machine's node is pinned to this one, which is never looser
than itself. A node run as a systemd unit
starts each holder in a scope of its own, `mycelium-pi-<held>.scope` in
`app.slice`, so the session outlives the node. A start placed on
another machine is asked of it through the link this node keeps to it,
without the tools its work prefers, since a node too old to read one
may have been preferred and would end the link rather than answer, and
its `attach` comes back as the words that join it over the dial,
as a held session's `attach` says, such as `mycelium --realm work
attach --on kestrel <held> -- ssh -t -- kestrel mycelium --realm work
attach <held>`. It fails with a reason when no link reaches the machine, its
mycelium is too old to start sessions, or it doesn't answer within 30
seconds.

A start that no machine of the realm takes has a machine made for it.
The node asks its providers what they offer and judges each offer as
it would a machine, by the facts it promises: a fact it doesn't promise
meets no need, and none is a machine already named. An offer whose
`hourly` is nothing, written with any number of zeros, may be made for
any start, so a provider lending machines that cost nothing is as easy
to start on as a machine of the realm. One that costs is made only for
a start that says it `may_cost` (`mycelium start --may-cost`); a start
that doesn't is refused, saying which offer would have done and what it
costs, and nothing is made. Of those that would take the work, it makes
one of the cheapest an hour, after taints and preferences, as
`PROGRAM make SHAPE --lasting SECONDS`: the work's
`lasts` and ten minutes to hand over, though no longer than the offer
lives, or as long as the offer lives where the work doesn't say. It
makes none where neither says, since no machine is made without an
end. The program is told who asked in `MYCELIUM_ASKER`: the address
of the session the kernel and pi's records prove asks, as a steer's
orchestrator is proved, as that session's own presence says it; else
`person` for the person's own login; else `unknown`, as for an agent
no session is proved for, a session whose presence says no address of
letters and digits (told in capitals), or any agent on a Mac, which
proves no session. The node's journal, and a refused make's error, say
why for each `unknown`. A provider serving only some refuses the rest
by failing its `make`, and nothing is made; it should refuse a `make`
where the variable is missing too, as it is from a node older than it,
though `offers`, `link` and `release` never carry it. A machine made
is booked, before it is dialled, for the asker its make was told: once
it links, a start or run is placed there only when that asker, or the
person, asks, so another agent's work goes elsewhere or waits, and a
machine whose book line names no asker takes only the person's. A
preview is held to it as its run would be, and an ordered carry there
of another's session is refused; only a start pinned to the machine
made for it is not held to it. The booking is the making node's, kept
while it runs and made again as it redials its book. Like the judging
of a run, this fences habit, not an agent set on passing as another,
which runs as the same account; an agent's process left with no agent
above it is judged the person's. A start relayed from another machine's
node makes nothing, since it is pinned to this machine by name, which no
offer meets. The program writes one JSON line,
`{"place", "machine", "until"}`:
where it is, as `--via` names it, the name its node will say for
itself, and when its provider ends it whatever else happens. The node
dials it as it would `--via PROGRAM:PLACE`, waits up to five minutes
for it to link, places the work again by the machine's own facts, and
starts it there. It lets the machine go, with `PROGRAM release PLACE`,
once its work has handed over, at `until`, or at once when nothing
started on it. Each make and release is kept as a JSON line in
`made.jsonl` in mycelium's folder, `{"at", "made", "realm"}` with the
machine, its place, shape, provider, `until` and hourly cost, and
`{"at", "released", "realm"}` with why, so what was made and for how
long can be summed. Every realm's node on the machine keeps the one
book, so each line names the realm whose node kept it. A node started
again links each machine its realm's lines say is made and not let go,
where it still asks the provider that made it, at its path or built
again into the Nix store (the same path past the store's hash), and
books as let go each whose `until` passed while it was down. A line
naming no realm, kept before lines named one, is never linked again.
A start whose providers make nothing says why: each machine's and each
offer's reasons, and each program that couldn't say what it offers.

`"Sprout"` starts a session of new work, as `mycelium sprout` asks, held
in no window: the node makes the `"Start"` and starts it as above, under
the same limits. `words` is the work, a link alone or words, which
become the session's first words; `from` the address of the cell it
comes from; `on` the machine, this one when it's none or names this
one; and all but `words` may be left out. Its folder is
`folder`, the checkout a link's repository has under home, or where the
asker is when that's inside a repository, never home itself, and the
session begins in it as the node names it, within home. Starting here,
the folder is judged on the disk as the asker sees it, never as the
node does, so an agent fenced from the person's folders can't have one
begun in them: each path is resolved inside the asker's root
(`/proc/<pid>/root`, its links kept inside it) and counts only where the
node's own path is the same file, and where the asker is comes from
`/proc/<pid>/cwd`. The asker is the process the kernel says is at the
socket's other end, trusted only while its start time is the one read
as it connected, so an id taken again is refused. Where that can't be
read, as on a Mac, once the asker has gone, or where the kernel can't
keep a path inside a root, the person's sprout is judged on the node's
disk from `cwd`, where the asker says it is, and an agent's is refused.
Refusals are errors saying why: `say what
the work is: a link or words`, `<folder> isn't a folder here`, a home
folder's, a folder with no repository's, or `an agent's sprout is judged
on the disk as it sees it, and <why it can't be read>; the person can
start it`. `mycelium sprout` judges the sprout on its own disk first, so
it refuses what the node would without asking it. A node from before
`"Sprout"` answers `not a request: unknown variant` naming it, and
`mycelium sprout` then asks for the `"Start"` it judged.

Where the judgement stops. It reads the asker's mount view only, so a
fence made of Landlock or file permissions is judged as though the
folder were reachable. The folder is judged by its path and begun in by
that path a moment later, so a folder the asker can write could be
swapped for a link in between. On another machine, the folder is that
machine's, and its node starts it as a `"Start"`, which judges no
folder; nor does a `"Start"` asked for directly. A `"Sprout"` is the
bound only for an asker that says one. The asker's start ticks are read
when the node begins serving its connection, not at the accept itself,
so an id taken again in that moment would pass; that is out of reach in
practice.

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

`"Run"` runs `command` on the machine placement chooses for `work`,
never on one made for it, in `folder` as for `"Start"`, through the
person's login shell there without a terminal, its kit's tools first on
its path as for `"Start"`, and with nothing on its
input. This machine runs it as the node's own; another, through the link
reaching it, as A Link says. The node judges who asks, as `by`,
`"Person"` or `{"Agent": kind}`: an agent, in the strictest fence the
presences of its process and its forebears say, and in a strict one
where it can't tell; an agent too where its process runs as one of
the machine's agents, may gain no privileges the node may, or sees
other mounts; else the person. A
`by` the request says only makes it stricter, and an agent runs
fenced at least. An agent's run goes only where an ordered carry could
land it (A Carry an Orchestrator Orders): placement is offered only
this machine and those, each other one missed as `looser-ground`,
`never`, saying why, which no `tolerates` lifts, and a `"Place"` an
agent asks is judged the same way, so where it is told the work would
go is where the work may run. Each machine so refused still counts for
what the person's lasting machines reach, and a placement's record
keeps it as `refused`, so a replay refuses it too.
There it runs as its
fence's declared `runs`, then the login shell, then `command`; a
machine declaring no `runs` for it refuses it, as does one whose
`runs` doesn't end with `--`, so nothing `command` says is read as
`runs`' own options. `runs` saying `--shared-tree`, or a shortening
of it, as a word of its own refuses every run, so pi-fence refuses a
run started in a work tree its asker doesn't hold. Only the node says
it, before the `--`, and only for an agent's run another machine
asked, whose folder, opened and named as the kernel names the open
folder, lies in a work tree the work broker records sessions as
holding, wherever the broker cut it, where pi's own record of every
one of them here says it works a quest whose ledger names the asking
machine. A ledger names a machine once it has said what it holds of
the quest, and goes on naming it until this machine stops sharing
the quest. A worker is heard only of the quests it was given or
began, so it names itself in no other ledger, while any other
machine's word is taken as the person's. Nothing a share carries
in, a README's sessions or trees among it, binds a tree to a quest,
and a quest shared only as one above another spares none. The run
starts in the folder opened, not by its path again, so a link
swapped since leads it nowhere else, and the record of runs keeps
that folder as `entered` and the quests as a list, `spared`. Any
other agent's run from another machine starting in a work tree, or
in a folder the node can't open, is refused, saying why there and in
the node's log, and runs nothing. A run another machine says the
person asked goes as the person, in no fence, so no tree is judged
for it. A run a client here says another machine asked is refused,
and logged, where the client is an agent's, as for `by`, since only
the node's link to that machine says so. A run is told no
`PI_SESSION_ID` but the session that asks it, where its process owns
that session alone, as the kernel and pi's records prove it, since
pi-fence grants the work trees that session holds. Alone counts
every session claimed there, proved or not, and every one its host's
record lists, so the sessions of a pi
host holding more than one, the person's, one on a Mac and one asked
from another machine are told none. The node takes `runs`
only from a declaration reached through folders and links root owns
and no one else may write, each followed as written, with any part
within the Nix store taken as the store's, and only where its first
word is in the Nix store; a link the account owns, even one into the
store, seals nothing. The person's run goes as the person. Once it
runs, the node answers `{"Running": {"machine"}}`, then a `Ran` a line:
`{"Out": base64}` and `{"Err": base64}` as it writes them, at most 64
KiB each, then `{"Exited": code}`, 128 and the signal where one ended
it, or `{"Failed": why}`. Each line is written before the next is
taken, so a client that reads slowly slows the run. A client that hangs
up stops it: its process group is sent the terminate signal, then the
kill signal if any of it still runs 10 seconds on. Each machine a run
crosses keeps it in `runs.jsonl` in mycelium's folder, and logs each
line: `{"run", "at", "by", "sessions", "from", "on", "folder",
"command", "spared"}` as it starts, `"spared"` only where the node
spared it a shared quest's tree, then `{"run", "at", "ended"}`, the last `Ran`
or `{"Stopped": why}` where its asker let go.
`mycelium run` prints what it writes as it comes and exits with its
code, or 255 when it couldn't run.

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
| `carry` with `quest` and `to` | `carried`, where its sessions went, as `mycelium carry` says it |

A record carries `key` (`session:<id>`, `machine:<name>`,
`message:<id>`, `team:<address>`, `network:<name>`, `cell:<name>`,
`unanswered:<id>`, `said:<id>`, `event:<session>:<at>`,
`receptor:<source>`, `builder:<name>`, `forage:<address>`,
`murmur:<cell>:<at>`, `tie:<cell>~<cell>`, `tie:<cell>` or
`ended:<session>`), `kind`,
`heard` (milliseconds since the epoch)
and the fields `Sessions`, `Machines` and `Messages` give it. A field
its source named `key`, `kind` or `heard`, such as a
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
message is heard when it was sent and never changes. The node holds
each message's `at` in UTC to the millisecond, as
`2026-11-16T17:00:00.000Z`, whatever offset or precision it came with,
so the order of the words is the order sent; an `at` that is no time
is held as when the node heard it. The `team` kind
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
by the very model `mycelium networks` draws, of the node's teams,
sessions, machines, receptors and builders, with its fields: its
`shape` (`orchestration`, `project`, `repository`, `tree`, `group`,
`machines` or `outside`), the `short` name a person types, its `title`
and `glyph`
where something says them, the network it is `within`, its `cells` and
the `machines` they run on, the machines' own network's `builders`
beside its machines, and `counts` of its sessions as
`needs-you`, `stuck`, `moving`, `quiet` and `dormant`. A tree is a
network once two cells or more hold it at once, as each session's
presence lists its `trees` (each `key`, `repo` and a worktree's
`branch` or a snapshot's `commit`), and stays one while any cell
holds it, though the node restarts. Its name is
`tree.<repo>:<branch>`, or `tree.<repo>:<commit>` for a snapshot, the
repository's whole key and the whole commit, read up to the last colon
since a branch holds none; its short name is the repository's last
part and the branch, `<name>@<branch>`, or the commit cut to seven,
saying the owner, `<owner>/<name>@<branch>`, or else the whole key,
where two trees' short names would be one. The machine is no part of
it: cells on two machines holding one branch share one tree. A tree one
cell holds alone, which no two have held at once, is no network, only
among that cell record's `trees`, and a group named as a tree joins
none. The `formed` kind holds each tree network formed, keyed by its
name, as the node keeps them in `formed-<realm>.json` beside
`carries.jsonl`: its `name`, `at`, when the node first saw two cells
hold it, and `left`, when it first saw the last cell gone, or null,
each in milliseconds since the epoch; it is heard at `left`, or else
`at`. The node looks each minute, and as `network`, `cell` or `formed`
is asked, so a tree two cells hold for less than a minute with nobody
asking may form no network. One no cell holds isn't drawn, and fades
ten minutes after `left`, so a cell back within them rejoins it; past
them none serves it, and a cell back alone forms it no more. A watch
of `formed` is told as the node sees one form, be left, be held again
or fade, so within the minute, or at once where a `network` or `cell`
ask saw it. Each node keeps its own, formed as its own
record of the realm holds them; a link doesn't carry them, though a
carried session's cell brings those its trees make. A file
that can't be read forms none. One that doesn't parse is moved aside
as the node starts, as `formed-<realm>.json.bad.<millis>`, so none
is lost under another. One that can't be opened, or moved, is left
as it is and never written, so what forms is kept only while the
node runs. A tree
whose every cell is parked counts them `dormant`, so it draws after
the living ones. A network is heard when the latest of its cells was,
or its team where its cells say nothing. Its records change as the
sessions do, so a machine come or gone shows with the next change to a
session. Ten thousand cells, each holding two trees, make their
networks within 30 MB, in time that grows with them.

The `cell` kind holds each cell of the realm, keyed by its name: a
session's address, a machine's name, or a receptor's source and machine,
such as `slack@kestrel`. Each is the very cell that
`mycelium networks -o json` prints, made by the same model from the
same records as the `network` kind, less its `kind`: its `type`
(`session`, `machine` or `receptor`), and where it says them its
`title`, a session's `id`, `window`, `standing` (`needs-you`, `stuck`,
`moving`, `quiet` or `dormant`), `doing`, `asks`, `repo`, `usage`, the
networks it `leads`, `machine`, `since`, `quest`, `plan`, `cwd` and the
`trees` it holds, a machine's or a receptor's `state`, a machine's
`busy` and the sessions it `runs`, a receptor's `counts`, and the
`networks` it joined. A session's `quest`
is the quest it works on, a field like any other; a cell is known by its
name alone. A builder is no cell: the machines' network names it among
its `builders`, and it is a `builder` record. A cell is heard when its
session's, machine's or receptor's record was, and its records change
as any of those do, so a machine's measure or a receptor's reading is
sent at once. A watch is sent only the cells that changed, each with
only the fields that moved. A node holds each record of every kind as
the bytes it is sent as, and reads only the fields that moved, so ten
thousand cells make their records within 30 MB while each holds two
trees, in time that grows with them.

The `tie` kind holds who talked with whom in the last day, made from the
messages the node holds: never what a message said, nor its id, only
who sent it, when, its `intent` and its groups. A message ties its
sender to each peer it names (`peer.<address>`) and to the sender of
the message it answers (`re`), where that is held, each once, and no
one to themself; one only to named groups, answering nothing, ties no
one. A cell is named by its session's own address wherever it goes by
another, and otherwise as said, uppercase. A tie is keyed by its two
ends sorted and joined, `tie:<A>~<B>`, which alone names them, so a
cell whose name holds a `~` is no cell: its messages are passed over
and no one is tied to it. A group's name never holds one. A tie
says each end's messages on it in the last hour as `hour` and in the
last day as `today`, in the order of its key, the `intents` and
`groups` its messages had, sorted, and its `strength`, and is heard at
its last message. Each message adds one to the strength, which halves every two
hours, said as it stood when the tie was heard rather than now, so a
tie changes only as its talk does; a reader fades it to now by
2^-((now - heard) / 2 h). A cell that talked or was talked to has a
record of its own, keyed by it, `tie:<A>`, saying every message it
sent in the last `hour` and `today`, to any group, and how many `ties`
it has, heard when it last sent or was talked to. A tie or a cell with
no message in a day is gone. The kind is told as each message comes
and each minute, as a message leaves an hour or a day. Ten thousand
cells with fifty thousand ties make their records within 30 MB, each
held as the bytes it is sent as, in time that grows with them. The
node folds the messages it holds once it lets them go, so a watch
holds it only to take them.

The `ended` kind holds each session of this machine's whose work
stopped while its quest goes on, keyed `ended:<session>`, so whoever
orchestrates the quest can be told. A session counts once this node
has seen its process ended for ten minutes, unparked or parked with
nothing left to wake it (its node doesn't say it stands `parked`),
and not carried from here in the last hour (unless the carry was
refused or came back) nor waiting its turn to resume, while its
presence was written within a day either way and its quest, the one
quest whose record attaches it in the quests this node serves, is
`active`, names an `orchestrator` that reads as an address, and has
no session of the realm running on it, though other quests attach
that one too. A session running there restarts the ten minutes.
Where the carries or the park markers can't be read, none is told,
and the node's log says so, naming the file, once while it runs. A
record says the
`session`, its `address`, the `quest`, the quest's `orchestrator`,
uppercase, `at`, when the session last changed, and `why`: `ended`, or
`parked-unwakeable`; it is heard when the node first saw the end. It
grants nothing, a notice to an address and never an order. It is gone
while a session runs on the quest, and comes back ten minutes after
the work stops again with the same `session` and `at`, which a teller
tells once; gone too once the quest is no longer active, or a day
after the node first saw it. Its quest and orchestrator change
only as the quests' record does. A node serving no quests holds none.
It is told as the sessions or the quests change, and each minute.

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

The `murmur` kind holds what this machine's cells murmured in the
quests' webs over the last day, keyed by the cell's address and its
`at` in seconds, a second murmur of that cell in that second keyed with
`.2` after it, the next `.3`, in the order said. Murmurs come from the
webs' event files today: each `<scratch>/<QUEST>/web/events/` file this
machine writes, read as `mycelium web fold` reads them, so the two
never disagree; a journey's or a landing's words are not murmurs yet.
A record says the `writer`, the cell that said it, the `machine`, `at`,
its `text`, what it is `tied_to` (each other end once, as said, from
any machine's ties), its `purpose`, what its cell's session says it
works on as the node holds it now, and how it was `settled`, in the
settling's words, those two null where it has none. A
murmur is heard at its `at`; its live fields are `text`, `tied_to`,
`purpose` and `settled`, and a purpose that moves shows with the next
murmur. A list, and a watch's first answer, give murmurs newest first.
A file is opened again only once its size or time written moves, and
read on from where it was, so ten thousand cells' murmurs are read
within 30 MB and one more is read in a small share of that time. The
node watches the nearest folder of each web there is, and reads them
again each minute for murmurs passing a day; `health` says a web it
can't read as the source `murmur`, `refused`.

The `receptor` kind holds each receptor of this machine, keyed by the
source it senses, as [Receptors](#receptors) says; its live fields are
`state`, `read_at` and `counts`.

The `forage` kind holds each forage of this machine's cells, keyed by
the cell's address, as [Forages](#forages) says; its live fields are
`purpose`, `status`, `documents`, `waits` and `lineage`.

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
fields: `mycelium networks` reads 41, about a quarter of a session's
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
2026-11-15, 128 KB of 207 KB was presence that hadn't moved. A field
that becomes null and one dropped are both said as null, so a reader
takes null for either. Asked again with another `deep`, in whatever
order, a watch starts over, its records whole. A
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
A `carry` is the person's one act to carry `quest` to machine `to`,
as a launcher asks it, and only the person's own login may ask it:
one an agent's process asks is `refused`, as a `"Free"` is. The node
finds the one machine `quest`'s sessions run or are parked on, then
runs the carry's steps itself against its own socket, or asks that
machine's node over its link with `checked` set. It is answered once
the carry has ended, the connection's other asks answered meanwhile.
A quest nothing runs, or one split across machines, is `refused`, as
is a carry whose step failed, saying which step and why.

The node also serves files another program keeps, each as a kind of its
own, so a reader asks the node and hears each change rather than reading
the files itself. `mycelium node --serves FILE` names them, in a JSON
object of sources by kind:

```json
{
  "quest": {"file": "/full/path/quests.json", "records": "quests", "key": "id"},
  "waiting": {"folder": "/full/path/waiting", "records": "items",
              "key": "id", "or": "title", "by": "quest"},
  "queue": {"file": "jobs/queue.json", "within": "runtime", "whole": true},
  "talk": {"talks": "/full/path/steer"}
}
```

A source is a `file`, or a `folder` whose `.json` files each hold some,
by its full path, or for one kept where it differs from one machine to
the next, by its path `within` the login's `runtime` folder, going down
from it. `records` names the field holding them: an array of
objects, each keyed by its `key` field, or an object of them, each keyed
by its own key. An array's source may name an `or` beside its `key`, the
field keying a record whose `key` is missing or empty. One whose `or` is
missing or empty too is keyed by its `key` even empty, so naming an `or`
refuses nothing that served without one; one with neither field refuses
its file, as one with no `key` does with no `or`. Of two records keyed
alike, the last is kept. A folder's file puts
its `by` field in front, so `waiting:QEST-A:login` is record `login` of
the file whose `quest` is `QEST-A`. A file that says how something
stands, rather than listing things, is held `whole` instead of naming
its `records`: one record, its id the machine's name, holding every
field of the file. One whose name ends `.jsonl` is of JSON lines, as a
program appending what happens keeps one: its record holds `lines`,
each whole line in the order written, as a talk's does. Each record's
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
`message`, `team`, `network`, `formed`, `cell`, `unanswered`, `said`,
`event`, `receptor`, `builder`, `carry`, `forage` or `murmur`, can't
be served.

A served record that waits on an act says which in `doneWhen`, an
object naming its `probe`, and the node judges it each time the
record is shown. A probe read from this machine or the record itself
is judged then; one that asks a program is asked off the reader's
way, at most every 30 seconds, four at a time, each answer held to
five seconds, its last answer standing meanwhile, so a new record it
asks of is shown until its first answer comes. Once the act is done,
the record is shown no more, and its kind's watchers hear it `gone`,
so nobody is asked for what they already did. A probe that says not
done, can't tell, or isn't known keeps it. While any act shown may
yet change, the kind's watchers are told again every 30 seconds, so
one done since is seen gone with no change to its file. The node's
own judgements, such as who orchestrates a quest, read every record.
A program that can't run is said in the node's log once, and every
wait on it stays. The probes:

- `rebooted`, with the `boot_id` of the boot it waits past, its 32
  hex digits in any case, with or without dashes: done once this
  machine's boot is another. A Mac names no boot, so it can't tell.
- `switch-done`, with the system it `wanted`, a store path: done once
  `/run/current-system` is that path.
- `pr-merged`, with its `repo`, as `owner/name`, and `number`: done
  once `gh pr view` says it merged. Only `host` `github`, the default,
  is asked. Each ask is one of the person's GitHub requests.
- `signed-in`, with its `tool`, `gh` or `gcloud`: done once the tool
  says the person is signed in, and asked again while done, as a
  sign-in lapses. What the tool writes is never read.
- `message-posted`, whose `verdict`, `{"done", "at"}`, the program
  that can read the channel writes on the record, as the node holds no
  token for it: believed for fifteen minutes after its `at`.

The talks of the steer inboxes are the one shape the node knows, as it
carries them itself. A source naming `talks`, the inboxes' folder and
nothing else, serves each `<ADDRESS>/replies/<id>.jsonl` as one record,
its id `<ADDRESS>/<id>`, holding its `address`, its `id` and its
`lines`: each whole line of JSON in the order written, a line still
being written left for the next read. Only each inbox's newest 20 talks
are served. The inboxes are watched to any depth, so a talk is heard as
its session writes each line, in an inbox made since.

### Receptors

A receptor senses one outside source for its person, such as Slack or
mail, intake only: nothing it runs sends, replies, reacts or marks
anything read. It says how it stands in a file of its own the node
serves as the `receptor` kind, and the networks draw it as a cell of
its source's `outside.<source>` network, beyond the machines.

The file is `receptors/<source>.json` in the realm's state folder,
written whole through a dot-file renamed into place, the folder `0700`
and the file `0600`. `mycelium receptors say SOURCE --state STATE
--every SECS --pid PID` writes it, with `--hour`, `--today` and
`--addressed` for its counts, and takes nothing else. It holds counts
and times alone, never what was read:

| Field | What it is |
| --- | --- |
| `source` | `slack`, `mail`, `calendar`, `calls`, `reviews`, `github` or `messages` |
| `realm` | the realm it senses for, which must be the node's |
| `state` | `reading`, `quiet`, `failing` or `off` |
| `since` | when the process keeping it began saying so |
| `read_at` | when it last read its source |
| `every` | how often it reads, from 1 second to a day |
| `pid`, `started` | the process keeping it, and that process's start, as below |
| `counts` | `hour`, `today`, and `addressed`, of today's those addressed to its person |

The record is an allow-list: a file holding any other field, a word
outside those listed, or a name that isn't its source's is malformed,
and the node leaves it out, as it does a file of another realm. So a
channel's name or a mail's subject can't reach the map by any field. No
count may pass `today`, and these bounds hold as a file is read as well
as written. A pid that isn't running is refused as it is written. A
process's start is kept as `started`, so a pid used again later can't
read as the receptor alive: on Linux in clock ticks since boot, as its
`stat` tells it, and on macOS in seconds since the epoch, as libproc
tells it. A record never leaves its machine, so the two never meet.
Elsewhere `started` is absent and the pid alone is judged.

The node reads only a regular file of at most 4 KiB, never following a
link and never waiting on a pipe, and reads each off its runtime, so
nothing planted in the folder can stall it. A receptors folder that is
a link is refused as it is written and left unread, and a dot-file
already standing where one is staged is removed rather than written
through.

The node judges each file rather than believing it, as it reads each
change to the folder and every 30 seconds besides: a receptor whose
process is gone, whose pid now names a process that started at another
time, or whose start was kept but can't be read now, as macOS keeps
some processes' starts from other accounts, is `off`, and one that read
nothing for three of its rounds is `failing`. Each record is heard at
its `read_at`, and says the `machine` it runs on.

#### Signals

When more is asked of its person, a receptor's node says so on the
receptor's network, as the receptor's own cell. The message is an `fyi`
from `<source>@<machine>`, the cell's name as the cells kind lists it, to
`#outside/<source>`, and its text is the source's name and its counts
alone: `github: 3 asked of you, up from 1; 5 today`. A climb in
`addressed` is said, a count that holds or falls isn't, a receptor first
seen says nothing, and a source says at most once in ten minutes, from
what it last said. A session that hears one weighs it as a fact: it
grants nothing, and it starts, moves or steers no cell.

Which sources may speak is the person's table, named by `mycelium node
--stimuli FILE` and shaped `{"say": ["github", "slack"]}`. The file is
taken only where no account but root could change it, walked link by
link as `--declared` is, or within the Nix store. A table unnamed,
refused or naming a source the node doesn't know leaves every source
silent. The node reads it once, as it starts, so a changed table takes
its restart.

Only the node speaks as a receptor, and a name reads as a receptor's
when the part before its `@` is a source's name once case, digits,
punctuation and spaces are set aside, or holds anything outside ASCII.
A client's message from any name with `@` in it is refused, and so is a
crossing from another realm's node that names a receptor. A link never
carries a receptor's message and refuses one it is handed. The node
serves a message from a receptor's name, on a watch or to a link, only
when it said it since it started, so after a restart its earlier
signals drop out of what it serves. What the node vouches for is what
it serves and carries: a reader of the messages folder itself, as a pi
session is, reads whatever the folder's account wrote there, under a
receptor's name or any other. A signal stays on its machine, as the
receptor's record does.

### Forages

A forage is a cell's own purpose, with the research, plans and reports
it keeps for it. The node serves one for each cell of this machine that
works on a quest, keyed by the cell's address and gone with the cell,
read from the machine's quest store. A quest is never a record of its
own, and nothing of a forage crosses a link: each node serves its own
cells', and a reader gathers them as `mycelium forage` does.

| Field | What it is |
| --- | --- |
| `cell`, `session`, `machine` | the cell, by its address, its session's id, and the machine it runs on |
| `quest` | the quest its purpose is written in, by id |
| `purpose` | the first sentence of the quest's Summary in its own words, at most 280 characters, or null |
| `status` | the quest's status, such as `active` |
| `documents` | each of its documents: `id`, `kind`, `title`, `stage`, and how many steps are `ticked` and `open` |
| `waits` | only when there are any, what the quest waits on its person for: `id`, `title`, `why`, `minutes`, `when`, whether it's `ready`, and when it was `raised` |
| `lineage` | `started_by`, the cell it answers to, and `parent`, the quest its own sits under |

A quest's id names a folder in the store, so one that isn't a plain
name of letters, digits, `-` and `_` is read as nothing, and a file is
read only when it's a regular file, never through a link. A quest the
store holds no record of gives its cells no forage.

The purpose is the quest's own words, so the node passes over what
isn't. Template guidance, a paragraph or a line that is one span in
`_` or `*` italics, is no purpose, and the words after it are. A
sentence ends at `.`, `!` or `?` before a space, but not inside paired
double quotes, where it ends at the closing quote when anything but a
lower-case letter follows. The first sentence is the purpose, a quote
in it kept whole, since a quest often opens with its person's own ask.
A Summary of nothing but guidance gives a `purpose` of null, which
`mycelium forage` says as "Its quest says no purpose yet." In the
front matter, a value left as YAML's null (`null`, `Null`, `NULL` or
`~`) says nothing, as an empty one does, while a quoted `"null"` is the
word. A `#` after a space or tab starts a comment, outside quotes.

Waits come from the launcher's waiting folder, `mission/waiting` under
the state home unless the node's `--waiting` names another: a file a
quest, named by the last part of its id, such as `KESTRL.json`,
holding `quest` and its `items`. The node reads it with the quest,
and again whenever a watch on the folder hears it change, or hears
the folder made anew. While it can watch at all, it also looks at the
folder every five seconds, since a watch can miss it made or removed,
as a Mac's was seen to, and reads it again then while no watch takes
it; every five minutes it reads each quest's waits again in any case.
A quest whose file is gone since the folder was last read, alone or
with the folder, reads its waits again. A file that is a link, longer
than 256 KB, not JSON, or naming no `quest` or another gives no
waits. An item without a `title` is left out, keeping the rest; one
without an `id`, or with an empty one, is known by its title, as the
launcher knows it, so two such items with one title share an id and
count as one wait, as the launcher counts them; and any other field
null or written wrong says nothing. Cleared waits aren't read, and a
forage whose quest waits on nothing has no `waits`. Beside the folder,
as the launcher keeps them, `settled.json` holds the waits its person
said are `done`, and `verdicts.json` the `items` the to do daemon
found `done` or `ready`, each by `<quest>:<id>`. A wait done either
way is left out, as the launcher leaves it out, and one found ready is
ready; one put off stays. Both are read again once changed, when the
folder is next looked at, and every quest held is read again with
them, since reading one reads them for all. One gone, a link, longer
than 8 MB, one that won't open, or saying it another way settles
nothing. One read whole but not JSON, as one caught halfway through
being written is, keeps what it last said whole until it changes
again, and on the node's first look has nothing to keep. The mesh's
own waits, the questions and holds still unanswered, are
`mycelium unanswered`'s.

A plan grows past 400 KB, so the node reads a quest off its runtime,
once a cell asks for it, and then only as it changes: a watch on the
quest's folder and its documents' folders hears each write, and a
document whose length and time are unchanged isn't read again unless a
watch heard it written. Every five minutes each quest held is looked at
again, for a change a watch missed, and those no cell works on any more
are let go. A quest is read on a shelf of its own, lent what the node
holds of it and taken back once read, so a forage asked for meanwhile
never waits on the disk. A presence beats every few seconds while its
cells seldom move, so forage watchers are told only when this
machine's cells came, went or moved quest, the beats within 250 ms
heard as one.

Ten thousand cells make their forages in time that grows with them,
within 64 MB. The target is 30 MB, the map's and the launcher's bound;
the gap is known, since each record carries its quest's documents,
about 5 KB a cell, made for each ask and never held. It closes by noon
on 18 November 2026, once the records are written as raw bytes, as the
cells' are.

## A Held Session

`mycelium holds add -- <program>` starts a program on a terminal that
a holder of its own keeps, so the window it started in can close and
`mycelium holds <id> attach` take it up again from any terminal on the
machine. `hold` is the same as `holds`, and the older spellings,
`hold -- <program>`, `held`, `attach <id>` and `again <id>`, still
work. The holder is its own process, in a session of its own, rather
than the node's: restarting the node's service stops everything the node
started, and a closing window hangs up on its own session only.

`mycelium holds <id> end` asks the node `{"End": {"held"}}` to end a
held session for good, on whichever machine holds it: here, or over the
link reaching that machine (`End` above), never by SSH. The person's
own login ends any. A session ends one only on the machine where both
run, once that node's record of orchestrations proves it orchestrates
the held session's quest, as an ordered carry is proved; a presence
naming its orchestrator proves nothing. Anyone else is refused, saying
why. An id held here is ended here; one held on more than one other
machine is refused. A holder that no longer answers on its socket has
its stale record taken away, and nothing it names is signalled. The
program's process group is told to end (`TERM`) and has ten seconds,
then its holder is told; what still runs is killed, and the answer's
`ending` says which, `gently` or `killed`. Its record and socket go,
and its presence leaves the sessions as its program's ends.

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
  already start sessions through it, and work needing a tool goes where
  the realm's kit holds it; next, a place says what it offers, such as
  evaluating and building for a system, so work that needs it is placed
  there, and a provider's offer promises the kit its machine would carry.
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
