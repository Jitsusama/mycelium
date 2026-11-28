# The Link's Tests, by Area

The link's tests past those in `../mod.rs`, one file to an area, each a
module of its `tests` that shares its helpers:

- `in_order.rs`: what a worker's end says of its sessions and their
  messages crosses in the order it was said, so the machine that made
  the worker judges each message against the presence its sender had:
  a session's last words go before its end, and its first after its
  presence.
