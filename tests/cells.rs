//! The node's cell records built for ten thousand cells within the bar
//! `realm` holds them to, as a program of its own, since only one may hold
//! the allocator counting its memory.

mod realm;

#[test]
fn ten_thousand_cells_make_their_cell_records_within_the_bar() {
    realm::holds_the_bar(
        realm::CELLS,
        "every session, machine and receptor is a cell",
        64_000_000,
        |holding| mycelium::node::cell_records(holding, &|_| None),
    );
}
