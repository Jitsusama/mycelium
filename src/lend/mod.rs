//! PROVISIONAL stand-in for KP0L6A's types, to be replaced by theirs.

pub mod serve;

use serde::{Deserialize, Serialize};

/// A model call lent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lend {
    /// The budget it is counted against.
    pub budget: Option<String>,
    /// Its method.
    pub method: String,
    /// Its path.
    pub path: String,
    /// Its headers.
    pub headers: Vec<(String, String)>,
    /// Its body, in base64.
    pub body: String,
}

/// What a lend answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lent {
    /// Its head.
    Head {
        /// Status.
        status: u16,
        /// Headers.
        headers: Vec<(String, String)>,
    },
    /// A piece of its body, in base64.
    Body(String),
    /// It ended whole.
    Done,
    /// It was refused.
    Refused {
        /// Status.
        status: u16,
        /// Why.
        why: String,
    },
}
