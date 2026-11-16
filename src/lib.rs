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
//! windows are kept by holders of their own, in [`hold`]; a conversation
//! a pi host holds among many is joined through the host, in
//! [`hosted_attach`]; one on another machine is joined from here again
//! each time the way there drops, in [`rejoin`]. What the person
//! says to a session from outside its window reaches its inbox through
//! [`steer`], on whichever machine it runs. A machine that says when it
//! leaves tells its own sessions the same way, before it does, and once
//! they are done and the machines it links with hold all it shares, a
//! machine that can release it does, in [`handover`]. A quest the person
//! takes to another machine goes there with its sessions, in [`carry`].
//!
//! Around them:
//!
//! - [`facts`]: what this machine is, as its node measures it
//! - [`flow`]: the one-way roads a realm's text may take into another
//! - [`room`]: the room it has for more work, as its queue says
//! - [`within`]: the power and room a machine within another has from it
//! - [`disk`]: how full its home's disk is, as its disk guard says
//! - [`own_health`]: how it stands, as its own health daemon says
//! - [`declared`]: what its configuration says of it
//! - [`reach`]: whether the networks it reaches only now and then answer
//! - [`placement`]: which machine work goes to, by what each says it is
//! - [`builders`]: the Nix builders it sends builds to, which are no
//!   machines of the realm
//! - [`offer`]: the shapes of machine a provider's program could make
//! - [`made`]: machines made from offers, and the book of each
//! - [`group`]: groups' names, patterns that hear them, and routes
//! - [`record`]: messages and sessions, as clients read them
//! - [`mesh`]: the harness mesh's files read as records
//! - [`state`]: what one node holds in memory
//! - [`unanswered`]: the questions waiting and holds standing on the mesh
//! - [`words`]: the person's own words to sessions, and what became of
//!   each
//! - [`node`]: the node itself
//! - [`served`]: files another program keeps, served as kinds of records
//! - [`swarm`]: the swarm's teams as a person reads them in a terminal
//! - [`networks`]: the cells and the networks they join, one model for
//!   every view
//! - [`machines`]: the realm's machines and builders as a person reads
//!   them in a terminal
//! - [`watching`]: watches of folders that hear only what changes them
//! - [`client`]: a client of the socket
//! - [`health`]: how a realm's network stands, for a daily read
//! - [`place`]: where a node keeps things on its machine
//! - [`awake`]: keeping its machine awake while it holds sessions
//! - [`install`]: whether the build a process runs is still installed
//! - [`ssh`]: what another machine's link key may run on this one
//! - [`update`]: its configuration's updater, asked to update it to main
//! - [`murmur`]: what this machine's cells murmured in the quests' webs,
//!   served as records
//! - [`web`]: a quest's shared space of murmurs and ties, one log folded

pub mod awake;
pub mod balance;
pub mod builders;
pub mod busy;
pub mod carry;
pub mod client;
pub mod daemon;
pub mod declared;
pub mod disk;
pub mod facts;
pub mod flow;
pub mod forage;
pub mod group;
pub mod handover;
pub mod health;
pub mod hindsight;
pub mod hold;
pub mod hosted_attach;
pub mod install;
pub mod link;
pub mod machines;
pub mod made;
pub mod mesh;
pub mod murmur;
pub mod networks;
pub mod node;
pub mod offer;
pub mod own_health;
pub mod place;
pub mod placement;
pub mod placements;
pub mod reach;
pub mod receptor;
pub mod record;
pub mod rejoin;
pub mod replay;
pub mod room;
pub mod run;
pub mod served;
pub mod share;
pub mod ssh;
#[cfg(test)]
mod stand_in;
pub mod state;
pub mod steer;
pub mod stimuli;
pub mod store;
pub mod swarm;
pub mod unanswered;
pub mod update;
pub mod watching;
pub mod web;
pub mod wire;
pub mod within;
pub mod words;
