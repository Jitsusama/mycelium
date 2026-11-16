//! The node's network records built for ten thousand cells within the bar
//! `realm` holds them to, as a program of its own, since only one may hold
//! the allocator counting its memory.

mod realm;

/// Half the cells to all of them at most three times as long. Ten
/// thousand cells' records are past any cache and a thousand's are not,
/// so from a tenth a busy builder took 15.2 and 15.6 times as long for
/// work that grows with them (2026-11-17), against 11.5 to 12.8 when
/// quieter. From half, where both are past it, work growing with the
/// cells takes twice as long and with their square four times, as a scan
/// of every session for each repository once did. The step at the cache's
/// edge is still told, each cell's time at a tenth beside the others, but
/// not held.
#[cfg(not(any(target_env = "musl", target_os = "macos")))]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 2,
    times: 3,
    within: None,
};

/// On macOS, whose allocator and a Mac lent to builds took 15.5 times as
/// long at a load of 95 to 130 (2026-11-16), at most twenty-five times.
/// Work that grows with the square of the cells and is a third of the
/// build takes about forty, so this still tells it.
#[cfg(target_os = "macos")]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 10,
    times: 25,
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
