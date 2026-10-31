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
//! - [`daemon`]: the same socket in the daemon interface every daemon on
//!   a machine speaks alike
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
//! - [`within`]: the power and room a machine within another has from it
//! - [`disk`]: how full its home's disk is, as its disk guard says
//! - [`declared`]: what its configuration says of it
//! - [`reach`]: whether the networks it reaches only now and then answer
//! - [`placement`]: which machine work goes to, by what each says it is
//! - [`offer`]: the shapes of machine a provider's program could make
//! - [`made`]: machines made from offers, and the book of each
//! - [`group`]: groups' names, patterns that hear them, and routes
//! - [`record`]: messages and sessions, as clients read them
//! - [`mesh`]: the harness mesh's files read as records
//! - [`state`]: what one node holds in memory
//! - [`node`]: the node itself
//! - [`served`]: files another program keeps, served as kinds of records
//! - [`client`]: a client of the socket
//! - [`place`]: where a node keeps things on its machine
//! - [`awake`]: keeping its machine awake while it holds sessions
//! - [`install`]: whether the build a process runs is still installed
//! - [`ssh`]: what another machine's link key may run on this one
//! - [`update`]: its configuration's updater, asked to update it to main

pub mod awake;
pub mod client;
pub mod daemon;
pub mod declared;
pub mod disk;
pub mod facts;
pub mod group;
pub mod handover;
pub mod hold;
pub mod install;
pub mod link;
pub mod made;
pub mod mesh;
pub mod node;
pub mod offer;
pub mod place;
pub mod placement;
pub mod reach;
pub mod record;
pub mod room;
pub mod served;
pub mod share;
pub mod ssh;
pub mod state;
pub mod steer;
pub mod store;
pub mod update;
pub mod wire;
pub mod within;
