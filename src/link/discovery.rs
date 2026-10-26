//! Discovery: which machines a node should link with, as they are found.
//! Today that is a list given to the node ([`Listed`]); machines found on
//! the network, or made by a provider, plug in behind the same trait.

use tokio::sync::mpsc;

use super::dial::Dial;

/// Tells a node of each machine to link with.
pub trait Discovery: Send + 'static {
    /// How each found machine is dialled.
    type Dial: Dial;

    /// Each machine as it is found; a machine found again is a new dial.
    fn found(self) -> mpsc::UnboundedReceiver<Self::Dial>;
}

/// Machines named up front.
#[derive(Debug, Clone)]
pub struct Listed<D>(pub Vec<D>);

impl<D: Dial> Discovery for Listed<D> {
    type Dial = D;

    fn found(self) -> mpsc::UnboundedReceiver<D> {
        let (sender, found) = mpsc::unbounded_channel();
        for dial in self.0 {
            // The receiver is held right here, so the send can't fail.
            let _ = sender.send(dial);
        }
        found
    }
}
