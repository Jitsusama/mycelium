//! The node's network records built for ten thousand cells within the bar
//! `realm` holds them to, as a program of its own, since only one may hold
//! the allocator counting its memory.

mod realm;

/// A tenth of the cells to all of them at most fifteen times as long.
/// Work that grows with them takes about ten times as long; work that
/// grows with their square takes more once it is a third of the build at
/// ten thousand, as a scan of every session for each repository once was.
#[cfg(not(target_env = "musl"))]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 10,
    times: 15,
    within: None,
};

/// Under musl, whose allocator slows as the heap grows, up to about
/// thirty times on a busy builder for work that grows with the cells, so
/// only work that grows with their square and is most of the build, about
/// a hundred, is told.
#[cfg(target_env = "musl")]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 10,
    times: 50,
    within: None,
};

#[test]
fn ten_thousand_cells_make_their_network_records_within_the_bar() {
    realm::holds_the_bar(
        realm::CELLS / 10,
        "about one network forms per ten cells",
        30_000_000,
        &GROWTH,
        |holding| mycelium::node::network_records(holding, &|_| None),
    );
}
