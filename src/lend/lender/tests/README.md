# The Lender's Tests, by Area

The lender's tests past those in `../tests.rs`, one file to an area, each
a module of `tests.rs` that shares its helpers:

- `asked_again.rs`: a lend its relay answers 421, asked once more on a
  fresh connection before any of its answer reaches the borrower, and
  counted and charged as one lend.
- `spending.rs`: what a model's lend may spend. The machine's cap holds
  through what each request reserves before it is sent, and each request
  is settled to what its answer says it used. Spend-watch's word on the
  budget it spends is tested here too.
