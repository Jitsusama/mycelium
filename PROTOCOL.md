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
| Shared files | quest folders, scratch | not yet; see Coming Next |
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

A session's own machine judges whether it lives, from its process. The
presence of a session elsewhere is kept with its process taken out, which
the mesh takes as there, and with `machine` naming where it runs. It never
goes over a session of this machine's or of a third machine's.

One link at a time carries a machine's sessions into a store: it claims
that machine first, by a lock in `~/.local/state/mycelium/claims/`, and a
second link to the same machine is refused until the first ends. The link
lets go of what it carried when the session ends or the link drops, and
what a link killed outright left behind is let go by the next to claim
that machine, or by the node as it starts and stops. While a link stands
it touches each file it carried once a minute, so a copy whose time has
stopped moving is one no link still vouches for.

A presence is carried only between the two machines it links, so every
pair of machines that should see each other's sessions links directly;
messages, being a union, reach every machine linked by any path.

## A Link

`link::run` runs one end of a link over any byte stream. Both ends run the
same code, each line a JSON value:

1. `{"Hello": {"machine", "realm", "version"}}`, first, from each end.
   Each end's membership admits the other or refuses it, and nothing else
   crosses until it has.
2. `{"Have": {"messages": [ids]}}`: every message the end holds from the
   past week, the mesh's own reach, so a link never hands back what the
   other end has let go.
3. `{"Message": {"id", "body"}}`: a message the other end lacks, first
   for each the Have didn't list, then each new one as it lands.
4. `{"Presence": {"session", "body"}}`: a living session of the sending
   end's machine, again whenever it changes.
5. `{"Ended": {"session"}}`: a session of the sending end's machine has
   ended.

Three seams shape a link:

- **Dialling** (`link::dial::Dial`): how a machine reaches another's end.
  Today, SSH: the other end is `mycelium link` run there, over the SSH
  session's pipes, and SSH has proved which machine it is.
- **Discovery** (`link::discovery::Discovery`): which machines to link
  with, as they are found. Today, a list given to the node.
- **Membership** (`link::membership::Membership`): which machines to admit.
  Today, a realm: any machine of the same realm speaking the same version,
  never the machine itself.

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

A watch sends the whole state first, `SessionsChanged` and
`MessagesChanged`, then each change: `SessionChanged`, `SessionForgotten`
and `Message`. A client that falls too far behind is let go, and connects
again for the whole state.

A group is named by dot-separated tokens: the mesh's `#quest/4uhc5q` is
`quest.4uhc5q`, and a peer's `@4UHC5Q` is `peer.4uhc5q`. A pattern takes
`*` for any one token and a final `>` for one or more.

## Coming Next

Each lands behind a seam above, or as a seam of its own:

- **Shared files:** quest folders and scratch the same on every machine,
  each version kept by its content's hash and the version it came from,
  so a write that crosses another keeps both and tells both writers.
- **The node's own log:** messages and presence numbered per machine, so
  catching up is one question, "what do you have from machine X after
  number N?", in place of the mesh's folders.
- **Connections of the node's own:** machines dialled by key, found on the
  network without a cloud, and encrypted always, behind `Dial` and
  `Discovery`; membership then checks the realm's enrolled keys.
- **Providers:** programs beside a node that make machines and start
  sessions on them, such as Aquifer's, which lives outside this
  repository.
