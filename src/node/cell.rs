//! The realm's cells as the daemon interface holds them: a record each,
//! keyed by its name, holding exactly the cell `mycelium networks -o json`
//! prints, made by the same model from the same records (`network::made`),
//! so a page drawing the map live and the command never differ.
//!
//! A builder is no cell here: the machines' network names it among its
//! `builders`, and it is a `builder` record of its own.
//!
//! Every cell is made again at each change, as the networks are, and a
//! watch is sent only the cells that changed (`daemon::changes`): ten
//! thousand build within 64 MB, in time that grows with them, as
//! `tests/cells.rs` shows.

use std::collections::BTreeMap;

use serde_json::Value;

use super::network::{self, Holding, Made, Standing};
use crate::daemon;
use crate::networks::Is;

/// The kind of a cell's records.
pub const KIND: &str = "cell";

/// The fields of a cell that change while it lives.
pub const LIVE: [&str; 18] = [
    "title", "window", "standing", "doing", "asks", "repo", "usage", "leads", "machine", "quest",
    "plan", "cwd", "trees", "state", "busy", "runs", "counts", "networks",
];

/// Every cell the node's records make, by key, each session standing as
/// `standing` says: its sessions', its machines' and its receptors'. Each
/// is heard when its own record was.
pub fn records(held: &Holding<'_>, standing: &Standing<'_>) -> BTreeMap<String, Value> {
    let Made {
        mut mycelium,
        heard,
    } = network::made(held, standing);
    // The networks are let go before the records are made, and each cell
    // as its record is, so no cell is held beside its record.
    let cells = std::mem::take(&mut mycelium.cells);
    drop(mycelium);
    cells
        .into_iter()
        .filter(|cell| cell.is != Is::Builder)
        .map(|cell| {
            let latest = heard.get(&cell.name).copied().unwrap_or_default();
            let record = network::record(KIND, &cell.name, latest, &cell);
            (daemon::key(KIND, &cell.name), record)
        })
        .collect()
}
