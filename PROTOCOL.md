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
| This machine's own | read markers, the queue, what is said to `#machine` | never |

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
4. `{"Presence": {"session", "body"}}`: a living session of the sending
   end's machine, again whenever it changes.
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

Only the quest's record crosses: its README, journey, log, documents and
attachments, never a lock, a staging file or a copy kept beside. The
scratch folder waits for a watch of its own, since a scratch can hold
hundreds of thousands of files.

Once a second, each end looks at its shared quests and says what changed,
as `Share` frames:

- `{"Shares": {"quests"}}`: every quest it shares. The other end starts
  sharing any it didn't, first moving its own copy aside to
  `~/.local/state/mycelium/before-share/<when>/<QUEST>/`.
- `{"Holds": {"quest", "files"}}`: each changed file's state.
- `{"Want": {"quest", "file"}}`: a file whose state is newer there.
- `{"File": {"quest", "file", "state", "body"}}`: the file, in base64,
  or no body once deleted. Its hash is checked before it is written.

A file changed on one end is taken by the other. One changed on both is
settled by the machine whose name sorts first, so the two never chase
each other: the journey and the log keep both ends' entries, and any
other file keeps the far end's beside it as `<file>.from-<machine>`.
A deletion gives way to a write. Record text crosses with the home folder
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
| `{"Messages": {"groups", "limit"}}` | `{"Messages": [message]}`, oldest first |
| `{"Send": {"from", "groups", "intent", "text", "re"}}` | `{"Sent": message}` |
| `{"Watch": {"groups", "recent"}}` | `"Watching"`, then events |

A session's `state` is `busy`, `idle`, `ended`, or `unreachable` while no
link reaches its machine.

A watch sends the whole state first, `SessionsChanged` and
`MessagesChanged`, then each change: `SessionChanged`, `SessionForgotten`
and `Message`. A client that falls too far behind is let go, and connects
again for the whole state.

A group is named by dot-separated tokens: the mesh's `#quest/4uhc5q` is
`quest.4uhc5q`, and a peer's `@4UHC5Q` is `peer.4uhc5q`. A pattern takes
`*` for any one token and a final `>` for one or more.

## Coming Next

Each lands behind a seam above, or as a seam of its own:

- **Shared scratch:** a quest's scratch folder shared beside its record,
  watched rather than looked at, with each file's mode kept, and a
  finding told to both writers when a file is kept beside another.
- **The node's own log:** messages and presence numbered per machine, so
  catching up is one question, "what do you have from machine X after
  number N?", in place of the mesh's folders.
- **Connections of the node's own:** machines dialled by key, found on the
  network without a cloud, and encrypted always, behind `Dial` and
  `Discovery`; membership then checks the realm's enrolled keys.
- **Providers:** programs beside a node that make machines and start
  sessions on them, such as Aquifer's, which lives outside this
  repository.
