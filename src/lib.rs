//! Mycelium: one small node per machine that, with the nodes of the other
//! machines in its realm, makes one network of agents talking and sharing
//! in groups.
//!
//! PROTOCOL.md says what crosses between machines and how. Each seam later
//! growth plugs into is a trait with one implementation today:
//!
//! - [`store`]: where a node keeps messages, presence and leases; the harness
//!   mesh's own folders today
//! - [`link`]: two machines' stores kept as one, with its own seams for
//!   how a machine is reached ([`link::dial`]), which machines to reach
//!   ([`link::discovery`]) and which to admit ([`link::membership`])
//! - [`wire`]: the local socket every client reads, pi, Quickshell or a
//!   terminal
//!
//! Shared files, a quest's record and scratch on every machine sharing it,
//! are kept by content in [`share`]. Sessions whose terminals outlive their
//! windows are kept by holders of their own, in [`hold`]. What the person
//! says to a session from outside its window reaches its inbox through
//! [`steer`], on whichever machine it runs. A machine that says when it
//! leaves tells its own sessions the same way, before it does, and once
//! they are done and the machines it links with hold all it shares, a
//! machine that can release it does, in [`handover`].
//!
//! Around them:
//!
//! - [`facts`]: what this machine is, as its node measures it
//! - [`room`]: the room it has for more work, as its queue says
//! - [`disk`]: how full its home's disk is, as its disk guard says
//! - [`declared`]: what its configuration says of it
//! - [`reach`]: whether the networks it reaches only now and then answer
//! - [`placement`]: which machine work goes to, by what each says it is
//! - [`group`]: groups' names, patterns that hear them, and routes
//! - [`record`]: messages and sessions, as clients read them
//! - [`mesh`]: the harness mesh's files read as records
//! - [`state`]: what one node holds in memory
//! - [`node`]: the node itself
//! - [`client`]: a client of the socket
//! - [`place`]: where a node keeps things on its machine

pub mod client;
pub mod declared;
pub mod disk;
pub mod facts;
pub mod group;
pub mod handover;
pub mod hold;
pub mod link;
pub mod mesh;
pub mod node;
pub mod place;
pub mod placement;
pub mod reach;
pub mod record;
pub mod room;
pub mod share;
pub mod state;
pub mod steer;
pub mod store;
pub mod wire;
