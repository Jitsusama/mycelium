//! The node's network records built for ten thousand cells within the bar
//! `realm` holds them to, as a program of its own, since only one may hold
//! the allocator counting its memory.

mod realm;

#[test]
fn ten_thousand_cells_make_their_network_records_within_the_bar() {
    realm::holds_the_bar(
        realm::CELLS / 10,
        "about one network forms per ten cells",
        30_000_000,
        |holding| mycelium::node::network_records(holding, &|_| None),
    );
}
