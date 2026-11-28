# The Node's Tests, by Area

The node's tests past those in `../mod.rs`, one file to an area, each a
module of its `tests` that shares its helpers:

- `from_demand.rs`: machines made from demand. A room is kept while
  cells run there and let go once it stands empty a while, makes are held
  to a budget, starts pack onto a room before any make, and a machine's
  world marks come from its maker, never from the machine.
