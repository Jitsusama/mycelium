//! Mycelium: one small node per machine that, with the nodes of the other
//! machines in its realm, makes one network of agents talking and sharing
//! in groups.
//!
//! PROTOCOL.md says what crosses between machines and how. Each seam later
//! growth plugs into is a trait with one implementation today:
//!
//! - [`store`]: where a node keeps messages and presence; the harness
//!   mesh's own folders today
//! - [`link`]: two machines' stores kept as one, with its own seams for
//!   how a machine is reached ([`link::dial`]), which machines to reach
//!   ([`link::discovery`]) and which to admit ([`link::membership`])
//! - [`wire`]: the local socket every client reads, pi, Quickshell or a
//!   terminal
//!
//! Around them:
//!
//! - [`group`]: groups' names, patterns that hear them, and routes
//! - [`record`]: messages and sessions, as clients read them
//! - [`mesh`]: the harness mesh's files read as records
//! - [`state`]: what one node holds in memory
//! - [`node`]: the node itself
//! - [`client`]: a client of the socket
//! - [`place`]: where a node keeps things on its machine

pub mod client;
pub mod group;
pub mod link;
pub mod mesh;
pub mod node;
pub mod place;
pub mod record;
pub mod state;
pub mod store;
pub mod wire;
