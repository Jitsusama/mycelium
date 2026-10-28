//! Shared files: a quest's record and scratch, the same on every machine
//! that shares the quest, so a session on any of them carries it on.
//!
//! What crosses is a quest's record and the folders of its scratch named
//! for sharing ([`tree`]).
//! Each file is kept by content, with a [`version::Version`] saying which
//! machines' changes it has seen. A newer version replaces a file whole;
//! two written apart are both kept and said to be ([`settle`]), except where a file's
//! own shape lets the two be merged, as a quest's journey's ([`journey`])
//! and its harness log's ([`log`]) do.

pub mod disk;
pub mod exchange;
pub mod journey;
pub mod ledger;
pub mod log;
pub mod pieces;
pub mod settle;
pub mod tree;
pub mod version;
pub mod watch;
