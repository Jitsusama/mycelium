//! How fast each large file crossed from the other end, said once it is
//! whole. On 27 November 2026 a session's log took 26 minutes to cross,
//! and nothing the node said named the link it came over or the rate it
//! came at, so which link was slow, and whether by its bytes or its turns,
//! could only be guessed.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

/// A large file crossing from the other end, as far as it has come.
struct Crossing {
    /// When its first piece was asked for.
    began: Instant,
    /// How many of its pieces have landed.
    pieces: u64,
    /// How many bytes of base64 those pieces were sent as.
    sent: u64,
}

/// The large files crossing from the other end, by quest and file, timed.
#[derive(Default)]
pub struct Rates {
    crossings: BTreeMap<(String, String), Crossing>,
}

/// Each piece that lands, as [`Rates::landed`] counts it.
pub struct Piece {
    /// Where in the file it begins.
    pub at: u64,
    /// How many bytes of base64 it was sent as.
    pub sent: u64,
    /// When the file's first piece was asked for, which times the crossing
    /// when this is that piece.
    pub asked: Instant,
}

impl Rates {
    /// Counts `piece` of file `key` of quest `id`; a first piece begins
    /// its crossing again, as a crossing asked again from its start does.
    pub fn landed(&mut self, id: &str, key: &str, piece: &Piece) {
        let file = (id.to_owned(), key.to_owned());
        if piece.at == 0 {
            let began = piece.asked;
            let crossing = Crossing {
                began,
                pieces: 0,
                sent: 0,
            };
            self.crossings.insert(file.clone(), crossing);
        }
        if let Some(crossing) = self.crossings.get_mut(&file) {
            crossing.pieces += 1;
            crossing.sent += piece.sent;
        }
    }

    /// How file `key` of quest `id` crossed, now it is whole at `len`
    /// bytes, or `None` for one whose first piece this end never saw.
    pub fn whole(&mut self, id: &str, key: &str, len: u64) -> Option<Crossed> {
        let crossing = self.crossings.remove(&(id.to_owned(), key.to_owned()))?;
        Some(Crossed {
            len,
            pieces: crossing.pieces,
            sent: crossing.sent,
            took: crossing.began.elapsed(),
        })
    }

    /// Forgets every crossing of quest `id`, which this end no longer takes.
    pub fn forget(&mut self, id: &str) {
        self.crossings.retain(|(quest, _), _| quest != id);
    }
}

/// How a large file crossed.
#[derive(Debug, PartialEq, Eq)]
pub struct Crossed {
    /// How long the file is.
    pub len: u64,
    /// How many pieces it crossed in.
    pub pieces: u64,
    /// How many bytes of base64 those were sent as, which a packed piece
    /// makes fewer than the file's own.
    pub sent: u64,
    /// How long it took, from its first piece's ask to its last landing.
    pub took: Duration,
}

impl fmt::Display for Crossed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let millis = self.took.as_millis().max(1);
        let rate = u128::from(self.len) * 1000 / millis;
        let link = u128::from(self.sent) * 1000 / millis;
        write!(
            f,
            "{} bytes in {} pieces in {:.1} s, {rate} bytes a second, \
             sent as {} bytes of base64 at {link} a second",
            self.len,
            self.pieces,
            self.took.as_secs_f64(),
            self.sent,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(at: u64, asked: Instant) -> Piece {
        Piece {
            at,
            sent: 1000,
            asked,
        }
    }

    #[test]
    fn a_crossing_is_timed_from_its_first_ask_and_counts_each_piece() {
        let mut rates = Rates::default();
        let asked = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("two seconds ago");
        for at in [0, 10, 20] {
            rates.landed(
                "QEST-20261107-TERN01",
                "record/harness.log",
                &piece(at, asked),
            );
        }
        let crossed = rates
            .whole("QEST-20261107-TERN01", "record/harness.log", 30)
            .expect("timed");
        assert_eq!((crossed.pieces, crossed.sent), (3, 3000));
        assert!(crossed.took >= Duration::from_secs(2), "{crossed:?}");
        assert_eq!(
            rates.whole("QEST-20261107-TERN01", "record/harness.log", 30),
            None,
            "it is said once"
        );
    }

    #[test]
    fn a_crossing_asked_again_from_its_start_is_counted_again() {
        let mut rates = Rates::default();
        let now = Instant::now();
        for at in [0, 10, 0, 10] {
            rates.landed(
                "QEST-20261107-TERN01",
                "record/harness.log",
                &piece(at, now),
            );
        }
        let crossed = rates
            .whole("QEST-20261107-TERN01", "record/harness.log", 20)
            .expect("timed");
        assert_eq!(crossed.pieces, 2);
    }

    #[test]
    fn a_crossing_whose_first_piece_never_landed_says_nothing() {
        let mut rates = Rates::default();
        rates.landed(
            "QEST-20261107-TERN01",
            "record/harness.log",
            &piece(10, Instant::now()),
        );
        assert_eq!(
            rates.whole("QEST-20261107-TERN01", "record/harness.log", 20),
            None
        );
    }

    #[test]
    fn a_quest_forgotten_forgets_its_crossings_alone() {
        let mut rates = Rates::default();
        for id in ["QEST-20261107-TERN01", "QEST-20261107-WREN01"] {
            rates.landed(id, "record/harness.log", &piece(0, Instant::now()));
        }
        rates.forget("QEST-20261107-TERN01");
        assert_eq!(
            rates.whole("QEST-20261107-TERN01", "record/harness.log", 10),
            None
        );
        assert!(
            rates
                .whole("QEST-20261107-WREN01", "record/harness.log", 10)
                .is_some()
        );
    }

    // The 27 November crossing, as it would have been said.
    #[test]
    fn a_crossing_says_its_rate_and_what_crossed_the_link() {
        let crossed = Crossed {
            len: 28_800_000,
            pieces: 440,
            sent: 38_400_000,
            took: Duration::from_secs(1560),
        };
        assert_eq!(
            crossed.to_string(),
            "28800000 bytes in 440 pieces in 1560.0 s, 18461 bytes a second, \
             sent as 38400000 bytes of base64 at 24615 a second"
        );
    }
}
