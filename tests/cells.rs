//! The node's cell records built for ten thousand cells within the bar
//! `realm` holds them to, as a program of its own, since only one may hold
//! the allocator counting its memory.

mod realm;

/// Half the cells to all of them at most three times as long, and all of
/// them within a second of the processor's time, a stop for the worst
/// only, since a busy builder took 188 ms to 510 ms for the same records.
/// The target, 100 ms on the machine the bar is set for, is said, not
/// held: the growth is what holds the time to the cells. Ten thousand
/// cells' records are past any cache, a thousand's are not, so the time
/// is first taken where both are: work growing with the cells takes twice
/// as long, and with their square four times.
///
/// The second is a release build's, which the package's own check tests,
/// as nix/checks.nix asserts. A debug build, as plain `cargo test` makes,
/// is held by its growth alone: its unoptimised code took 1.5 s for the
/// same records on a cloud builder on 25 November 2026, on main as on
/// every branch. tests/ties.rs holds its bar the same way.
#[cfg(not(target_env = "musl"))]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 2,
    times: 3,
    within: if cfg!(debug_assertions) {
        None
    } else {
        Some(std::time::Duration::from_secs(1))
    },
};

/// Under musl, whose allocator slows as the heap grows, held as the
/// networks are there: a tenth of the cells to all of them at most fifty
/// times as long, since how musl grows from half of them is not measured.
#[cfg(target_env = "musl")]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 10,
    times: 50,
    within: None,
};

#[test]
fn ten_thousand_cells_make_their_cell_records_within_the_bar() {
    realm::holds_the_bar(
        realm::CELLS,
        "every session, machine and receptor is a cell",
        // Each record held as the bytes it is sent as, while each cell
        // holds two trees and three in ten say subjects (realm's
        // `subjects`).
        30_000_000,
        &GROWTH,
        |holding| mycelium::node::cell_records(holding, &|_| None),
    );
}
