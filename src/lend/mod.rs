//! A model call one machine lends another: the borrower's pi asks as it
//! would ask a model's relay, and the lender makes the call with its own
//! credentials, which never leave it, answering as the model does. Each
//! crosses the link both machines' nodes already share.
//!
//! [`serve`] is the borrower's side, where pi's requests come in, and
//! [`lender`] the lender's, where they are carried to its model relay.

pub mod forge;
pub mod lender;
pub mod serve;

use serde::{Deserialize, Serialize};

/// How many [`Lent`]s wait between a lend and its reader at most, at each
/// step from the lender to whoever asked for it.
pub const BACKLOG: usize = 4;

/// A model call asked of the machine lending, as pi asked it, with none
/// of pi's credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lend {
    /// The budget the lender counts it against, where it keeps more than
    /// one; none for its default.
    pub budget: Option<String>,
    /// Its HTTP method.
    pub method: String,
    /// Its path, as pi asked it, such as `/apis/anthropic/v1/messages`.
    pub path: String,
    /// The headers a model's answer turns on, and no others.
    pub headers: Vec<(String, String)>,
    /// Its body, in base64.
    pub body: String,
}

/// A piece of what a lend answers, in the order the lender says them: a
/// head, then its body's pieces, then that it is done; or, before any
/// head, that it is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lent {
    /// The answer's status and headers.
    Head {
        /// Its HTTP status.
        status: u16,
        /// Its headers, as the model gave them.
        headers: Vec<(String, String)>,
    },
    /// A piece of the answer's body, in base64.
    Body(String),
    /// The answer ended whole.
    Done,
    /// The lend was refused, or failed before its head.
    Refused {
        /// The HTTP status pi is answered with: 429 past the day's count,
        /// 403 for a machine cut off or a scope it may not ask, 502 where
        /// the model failed, 503 where no link reaches the lender.
        status: u16,
        /// Why, in words pi shows.
        why: String,
    },
}

impl Lent {
    /// Whether nothing follows it.
    #[must_use]
    pub fn ends(&self) -> bool {
        matches!(self, Self::Done | Self::Refused { .. })
    }
}
