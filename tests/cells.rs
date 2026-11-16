//! The node's cell records built for ten thousand cells within the bar
//! `realm` holds them to, as a program of its own, since only one may hold
//! the allocator counting its memory.

mod realm;

/// Half the cells to all of them at most three times as long, and all of
/// them within half a second of the processor's time. Ten thousand cells'
/// records are past any cache, a thousand's are not, so the time is first
/// taken where both are: work growing with the cells takes twice as long,
/// and with their square four times.
#[cfg(not(target_env = "musl"))]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 2,
    times: 3,
    within: Some(std::time::Duration::from_millis(500)),
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
        64_000_000,
        &GROWTH,
        |holding| mycelium::node::cell_records(holding, &|_| None),
    );
}
