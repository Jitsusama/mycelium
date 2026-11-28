//! The lends this machine's node is answering, counted so its stop lets
//! each end before the node does. A node restarting once dropped every
//! model call in flight at once, each already charged, and its rooms paid
//! again for each retry. From the stop no new lend is taken, each in
//! flight runs on for at most a bound, and what is left then is cut,
//! saying so in its line.

use std::time::Duration;

use tokio::sync::watch;

/// How long a node's stop waits for its lends in flight, unless its
/// command line says otherwise. systemd's stop timeout is 90 s by
/// default, as on the units that lend, so a minute leaves the rest of the
/// stop, its links' ends and the store's sweep, time to run before the
/// service manager kills the node.
pub const LENDS_DRAIN_WITHIN: Duration = Duration::from_secs(60);

/// Why a lend asked once the node is stopping is refused, with a 503,
/// which pi asks again on, as it does for a lender no link reaches.
pub(super) const STOPPING: &str =
    "this machine's node is stopping, so it takes no new lend; ask again soon";

/// What a lend's line says of one the node's stop cut, so a reader can
/// tell it from one whose borrower stopped reading.
pub(super) const CUT: &str = "cut by the node's stop";

/// Why the borrower of a lend the node's stop cut hears it refused.
pub(super) const CUT_WHY: &str = "this machine's node stopped before the lend ended";

/// How the lends stand.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Standing {
    /// How many were taken and haven't yet written their lines.
    in_flight: usize,
    /// Whether the node's stop has begun, so no more are taken.
    pub(super) stopping: bool,
    /// Whether the stop cut those left.
    cut: bool,
}

/// The lends a lender is answering, as its node's stop waits on them.
pub(super) struct Lends(pub(super) watch::Sender<Standing>);

impl Default for Lends {
    fn default() -> Self {
        Self(watch::Sender::new(Standing::default()))
    }
}

/// One lend taken, counted until it is dropped.
pub(super) struct InFlight(watch::Sender<Standing>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.send_modify(|standing| {
            standing.in_flight = standing.in_flight.saturating_sub(1);
        });
    }
}

impl Lends {
    /// Takes a lend, counted for as long as what this answers lives;
    /// none once the node's stop has begun.
    pub(super) fn begun(&self) -> Option<InFlight> {
        let mut taken = false;
        self.0.send_if_modified(|standing| {
            taken = !standing.stopping;
            standing.in_flight += usize::from(taken);
            taken
        });
        taken.then(|| InFlight(self.0.clone()))
    }

    /// Whether the node's stop cut the lends left.
    pub(super) fn is_cut(&self) -> bool {
        self.0.borrow().cut
    }

    /// Resolves once the node's stop cuts the lends left; never before.
    pub(super) async fn cut(&self) {
        let mut standing = self.0.subscribe();
        // The sender is this one's own, so the wait ends only by the cut.
        let _ = standing.wait_for(|standing| standing.cut).await;
    }

    /// Takes no lend from now, then waits for those in flight to end: at
    /// most `within`, past which those left are cut and waited for as
    /// they write their lines, or until `again`, a second stop, which
    /// cuts them and ends the wait at once. With none in flight it ends
    /// at once.
    pub(super) async fn drain(&self, within: Duration, again: impl Future<Output = ()>) {
        let mut standing = self.0.subscribe();
        self.0.send_modify(|standing| standing.stopping = true);
        let taken = standing.borrow_and_update().in_flight;
        if taken == 0 {
            return;
        }
        let began = tokio::time::Instant::now();
        tokio::pin!(again);
        let ended = |standing: &Standing| standing.in_flight == 0;
        let cut = tokio::select! {
            _ = standing.wait_for(ended) => {
                let took = began.elapsed().as_millis();
                eprintln!("stopping: its {taken} lends in flight ended in {took} ms");
                return;
            }
            () = tokio::time::sleep(within) => true,
            () = &mut again => false,
        };
        let left = self.cut_left();
        if !cut {
            eprintln!("stopping: stopped again, so its {left} lends in flight left were cut");
            return;
        }
        // A cut lend ends at its next turn, so this waits only for each to
        // write its line, unless the node is stopped again meanwhile.
        tokio::select! {
            _ = standing.wait_for(ended) => {}
            () = again => {}
        }
        let bound = within.as_secs();
        eprintln!("stopping: {left} of its {taken} lends in flight were cut at {bound} s");
    }

    /// Cuts the lends left, answering how many there are.
    fn cut_left(&self) -> usize {
        let mut left = 0;
        self.0.send_modify(|standing| {
            standing.cut = true;
            left = standing.in_flight;
        });
        left
    }
}
