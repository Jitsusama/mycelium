//! How fast a node's links send in bulk: one byte rate shared by every
//! link of the node, so links catching up together never fill the
//! machine's uplink.
//!
//! On 27 November a node restarted and its links to about ten cloud
//! machines each sent the other end every message it lacked as fast as
//! the pipe took them, for about 15 minutes, filling a 10.8 Mbit/s
//! uplink; the model relay on the same uplink went from 1 to 3
//! connection resets a minute to about 40. What a link owes in bulk (the
//! messages a catch-up offers, and what it says of the quests it shares)
//! now waits in [`Owed`] and crosses a frame at a time as [`Pace`] lets
//! it, between the link's other work. What a person or a session waits
//! on (asks, answers, lends, runs' output, presence, a new message) never
//! waits on the pace, so pacing never slows a model call or a run.
//!
//! The pace is a generic cell rate: each frame sent pushes the time the
//! pace is next free on by its bytes over the rate, and a frame may go
//! once that time is no more than [`BURST`] ahead of now. Two links
//! asking at once on two threads may each send one frame past it, so the
//! rate holds to within a frame for each link.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use super::Frame;

/// What every link of a node sends in bulk at most together, in bits a
/// second, unless the node is told otherwise: 2 Mbit/s, which leaves
/// most of a 10 Mbit/s uplink to what people and sessions wait on.
pub const BULK_BITS_A_SECOND: u64 = 2_000_000;

/// How far ahead of the rate the links may send at once, so a few frames
/// sent together after a quiet while don't each wait their turn.
const BURST: Duration = Duration::from_millis(250);

/// A byte rate shared by every link given a clone of it.
#[derive(Clone, Debug)]
pub struct Pace {
    /// Bytes a second, and when the pace is next free; none for a pace
    /// that never holds anything back.
    shared: Option<Arc<Mutex<Bucket>>>,
}

#[derive(Debug)]
struct Bucket {
    bytes_a_second: u64,
    free_at: Instant,
}

impl Pace {
    /// A pace of `bits_a_second` for every link it is given to together;
    /// none, which holds nothing back, for 0.
    #[must_use]
    pub fn at_most(bits_a_second: u64) -> Self {
        let bytes_a_second = bits_a_second / 8;
        Self {
            shared: (bytes_a_second > 0).then(|| {
                Arc::new(Mutex::new(Bucket {
                    bytes_a_second,
                    free_at: Instant::now(),
                }))
            }),
        }
    }

    /// A pace that holds nothing back, for a link with no node behind it.
    #[must_use]
    pub fn unpaced() -> Self {
        Self { shared: None }
    }

    /// Waits until the pace lets another frame go. Nothing is taken from
    /// the pace by waiting, so it is cancel safe, as `select!` needs.
    pub async fn ready(&self) {
        let Some(shared) = &self.shared else {
            return;
        };
        loop {
            let free_at = lock(shared).free_at;
            let now = Instant::now();
            if free_at <= now + BURST {
                return;
            }
            tokio::time::sleep_until(free_at - BURST).await;
        }
    }

    /// Takes `bytes` sent from the pace, which pushes on when it is next
    /// free for every link sharing it.
    pub fn spend(&self, bytes: usize) {
        let Some(shared) = &self.shared else {
            return;
        };
        let mut bucket = lock(shared);
        let now = Instant::now();
        let taken = taken_by(bytes, bucket.bytes_a_second);
        bucket.free_at = bucket.free_at.max(now) + taken;
    }
}

/// How long `bytes` take at `bytes_a_second`.
fn taken_by(bytes: usize, bytes_a_second: u64) -> Duration {
    let nanos = u128::try_from(bytes)
        .unwrap_or(u128::MAX)
        .saturating_mul(1_000_000_000)
        / u128::from(bytes_a_second.max(1));
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// The bucket, whose every change is a whole assignment, so one a
/// panicking holder left is still whole.
fn lock(shared: &Mutex<Bucket>) -> std::sync::MutexGuard<'_, Bucket> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One thing a link owes the other end in bulk.
#[derive(Debug)]
pub(super) enum Owing {
    /// A message a catch-up offered, by its id, read from the store only
    /// as it goes, so a week of messages owed holds only their ids.
    Message(String),
    /// A frame said already, such as a share, kept in order with the rest.
    Frame(Box<Frame>),
}

/// What a link owes the other end in bulk, in the order it was owed.
#[derive(Debug, Default)]
pub(super) struct Owed {
    owing: VecDeque<Owing>,
    /// The messages among them, so a catch-up owing one already owed
    /// doesn't owe it twice.
    messages: HashSet<String>,
    /// How many frames are owed, so a frame that must cross in order with
    /// them goes behind them.
    frames: usize,
    /// Whether a catch-up owes messages still owed, so the link counts
    /// itself caught up only once they have gone.
    catching: bool,
}

impl Owed {
    /// Whether nothing is owed.
    pub(super) fn is_empty(&self) -> bool {
        self.owing.is_empty()
    }

    /// Whether any frame is owed.
    pub(super) fn owes_frames(&self) -> bool {
        self.frames > 0
    }

    /// Marks a catch-up as having owed what it lacked, and says whether
    /// it is caught up already, owing no message.
    pub(super) fn caught_up(&mut self) -> bool {
        self.catching = true;
        self.drained()
    }

    /// Whether the messages a catch-up owed have all gone since it was
    /// last asked, which it says once.
    pub(super) fn drained(&mut self) -> bool {
        let drained = self.catching && self.messages.is_empty();
        self.catching &= !drained;
        drained
    }

    /// Owes the message `id`, unless it is owed already.
    pub(super) fn message(&mut self, id: String) {
        if self.messages.insert(id.clone()) {
            self.owing.push_back(Owing::Message(id));
        }
    }

    /// Owes `frame`, behind everything owed before it.
    pub(super) fn frame(&mut self, frame: Frame) {
        self.frames += 1;
        self.owing.push_back(Owing::Frame(Box::new(frame)));
    }

    /// The first thing owed, taken off.
    pub(super) fn next(&mut self) -> Option<Owing> {
        let next = self.owing.pop_front()?;
        match &next {
            Owing::Message(id) => {
                self.messages.remove(id);
            }
            Owing::Frame(_) => self.frames -= 1,
        }
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_pace_lets_a_burst_go_and_then_holds_to_its_rate() {
        // 8,000 bits a second is 1,000 bytes a second.
        let pace = Pace::at_most(8_000);
        let began = Instant::now();
        pace.ready().await;
        pace.spend(250);
        pace.ready().await;
        assert_eq!(began.elapsed(), Duration::ZERO, "its burst goes at once");
        pace.spend(1_000);
        pace.ready().await;
        assert_eq!(
            began.elapsed(),
            Duration::from_millis(1_000),
            "and then each byte waits its turn"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_pace_of_nothing_holds_nothing_back() {
        let began = Instant::now();
        for pace in [Pace::unpaced(), Pace::at_most(0)] {
            pace.spend(1 << 30);
            pace.ready().await;
        }
        assert_eq!(began.elapsed(), Duration::ZERO);
    }

    #[test]
    fn a_message_owed_twice_is_sent_once_in_its_first_place() {
        let mut owed = Owed::default();
        owed.message("1-WREN01-1-1".to_owned());
        owed.frame(Frame::HandedOver);
        owed.message("1-WREN01-1-1".to_owned());
        assert!(
            !owed.caught_up(),
            "a catch-up owing a message isn't caught up"
        );
        assert!(matches!(owed.next(), Some(Owing::Message(_))));
        assert!(owed.drained(), "until it has gone");
        assert!(!owed.drained(), "which is said once");
        assert!(owed.owes_frames());
        assert!(matches!(owed.next(), Some(Owing::Frame(_))));
        assert!(!owed.owes_frames());
        assert!(owed.next().is_none(), "owed once");
        owed.message("1-WREN01-1-1".to_owned());
        assert!(
            matches!(owed.next(), Some(Owing::Message(_))),
            "and owed again once sent"
        );
    }
}
