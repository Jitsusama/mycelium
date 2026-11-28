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

/// How long a stop waits, once it has cut the lends left, for each to
/// write its line. A cut lend ends at its next turn, so this is only a
/// guard against one that never takes it, which would otherwise hold the
/// stop until the service manager kills the node.
pub(super) const CUT_LINES_WITHIN: Duration = Duration::from_secs(5);

/// Why a lend asked once the node is stopping is refused, with a 503.
/// A borrowing node that hears it asks again, as it does a lender no
/// link reaches, rather than handing it to pi, since nothing of it went
/// upstream; pi asks again on it too, from a node too old to.
pub const STOPPING: &str =
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
    /// most `within`, past which those left are cut, or until `again`, a
    /// second stop, which cuts them at once. Either way it then waits as
    /// long as [`CUT_LINES_WITHIN`] for each cut lend to write its line.
    /// With none in flight it ends at once.
    pub(super) async fn drain(&self, within: Duration, again: impl Future<Output = ()>) {
        let mut standing = self.0.subscribe();
        self.0.send_modify(|standing| standing.stopping = true);
        let taken = standing.borrow_and_update().in_flight;
        if taken == 0 {
            return;
        }
        let bound = within.as_secs();
        eprintln!("stopping: waiting as long as {bound} s for its {taken} lends in flight");
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
        // Waited for after a second stop too, so a cut lend's line and its
        // settlement are kept however the stop ended.
        let written = tokio::select! {
            written = tokio::time::timeout(CUT_LINES_WITHIN, standing.wait_for(ended)) => {
                written.is_ok()
            }
            () = &mut again, if cut => false,
        };
        let how = if cut {
            format!("at {bound} s")
        } else {
            "as it was stopped again".to_owned()
        };
        let unwritten = if written {
            ""
        } else {
            ", not all of them writing their lines in time"
        };
        eprintln!("stopping: {left} of its {taken} lends in flight were cut {how}{unwritten}");
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
