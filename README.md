# Mycelium

A nimble core for agent networks. One small node runs on each host, whether
it is the machine you sit at, one on your network or one in a cloud, and the
nodes together make one network of agents that talk and share files in
spaces. A node with no peers is the whole network, so nothing here needs a
cloud.

Nothing is built yet. The protocol comes first, in `PROTOCOL.md`, and the
node follows it.

## What It Is For

- **Spaces:** agents attach to a space to hear its feed and share its folder,
  either woken by each post or reading quietly.
- **Hosts as equals:** local, networked and cloud hosts run the same node and
  join by enrolling a key. Other kinds of host, such as hosted sandboxes,
  plug in through providers that live outside this repository.
- **Seen from anywhere:** each node serves one local socket of JSON lines: a
  watch answers with the whole state and then each change. A status bar, a
  terminal command or an agent harness reads it the same way.
- **Sessions held by the node:** an agent's terminal outlives its window and
  its link, and any terminal on any host can attach to it.
- **Realms apart:** each realm is its own network with its own keys, and
  nothing crosses between realms.

## Lineage

An earlier experiment of the same name proved the concept on NATS. This core
keeps the concept and none of the code.
