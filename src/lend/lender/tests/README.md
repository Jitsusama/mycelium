# The Lender's Tests, by Area

The lender's tests past those in `../tests.rs`, one file to an area, each
a module of `tests.rs` that shares its helpers:

- `spending.rs`: what a model's lend may spend. The machine's cap holds
  through what each request reserves before it is sent, and each request
  is settled to what its answer says it used. Spend-watch's word on the
  budget it spends is tested here too.
