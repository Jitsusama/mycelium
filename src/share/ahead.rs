//! The pieces of a file asked ahead of what is staged, so several cross in
//! each round trip rather than one.
//!
//! Each piece waits at both ends of a link behind whatever the link is
//! doing when it comes, as a look over every shared quest, so with one
//! piece asked at a time a large file crossed at one piece a round trip
//! however fast the link: on 27 November a carried session's 28.8 MB log
//! took 26 minutes. Pieces asked ahead are answered in turn from the
//! sender's queue, so a wait at either end holds up the few in flight
//! together rather than each piece alone. A link carries its frames in the
//! order they were said, so the pieces still come in turn and are staged
//! as they always were; one that doesn't follow what is staged is passed
//! over as before, and the whole file's hash is checked once it lands.

use std::collections::BTreeMap;

use super::pieces::PIECE;

/// The most asked ahead over a link at once, in bytes, beyond the one
/// piece each crossing always has asked: eight of this build's pieces.
/// Kept for the link as a whole rather than for each file, since every
/// file of a turn may be crossing at once, and the slowest link must carry
/// what a turn asks within the asker's wait. A sender's piece longer than
/// this, as an older end's 1 MiB, is asked one at a time, as it always was.
pub const AHEAD: u64 = 8 * PIECE;

/// How far one file's crossing has been asked and staged.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Crossing {
    /// The content it crosses, by hash, so a crossing of other content
    /// begun over it starts its own.
    hash: String,
    /// The length of the sender's pieces, as its first piece was.
    piece: u64,
    /// How long the whole file is.
    len: u64,
    /// Where what is staged ends.
    staged_to: u64,
    /// Where the asks so far end.
    asked_to: u64,
}

impl Crossing {
    /// The bytes asked and not yet staged.
    fn in_flight(&self) -> u64 {
        self.asked_to.saturating_sub(self.staged_to)
    }

    /// The offsets to ask now that what is staged ends at `next`, with
    /// `others` asked of the link's other crossings: one piece always,
    /// and more while the link has fewer than [`AHEAD`] bytes asked, never
    /// past the file's end.
    fn ask(&mut self, next: u64, others: u64) -> Vec<u64> {
        self.staged_to = next;
        self.asked_to = self.asked_to.max(next);
        let most = AHEAD.max(self.piece);
        let mut asks = Vec::new();
        while self.asked_to < self.len
            && (self.in_flight() == 0 || others + self.in_flight() < most)
        {
            asks.push(self.asked_to);
            self.asked_to = self.asked_to.saturating_add(self.piece);
        }
        asks
    }
}

/// The crossings from one machine, by quest and file.
#[derive(Debug, Default)]
pub struct Ahead {
    crossings: BTreeMap<(String, String), Crossing>,
}

/// One piece staged: where it was in the file, how long it was, and how
/// long the whole file is.
#[derive(Debug, Clone, Copy)]
pub struct Landed {
    /// Where in the file it began.
    pub at: u64,
    /// How many bytes it held.
    pub piece: u64,
    /// How long the whole file is.
    pub len: u64,
}

impl Ahead {
    /// The offsets of file `key` of quest `id`, whose content is `hash`,
    /// to ask now that the piece `landed` was staged and the file needs
    /// more from `next`. The first piece sets the crossing's piece length;
    /// each piece asks as much more as the link's budget leaves. A piece
    /// of no crossing this end knows, as one of content changed since,
    /// asks only from `next`, as one always did.
    pub fn asks(&mut self, id: &str, key: &str, hash: &str, landed: Landed, next: u64) -> Vec<u64> {
        let file = (id.to_owned(), key.to_owned());
        if landed.at == 0 {
            self.crossings.remove(&file);
            let others = self.in_flight();
            let mut crossing = Crossing {
                hash: hash.to_owned(),
                piece: landed.piece.max(1),
                len: landed.len,
                staged_to: next,
                asked_to: next,
            };
            let asks = crossing.ask(next, others);
            self.crossings.insert(file, crossing);
            return asks;
        }
        let others = self
            .crossings
            .iter()
            .filter(|(other, _)| **other != file)
            .map(|(_, crossing)| crossing.in_flight())
            .sum();
        match self.crossings.get_mut(&file) {
            Some(crossing) if crossing.hash == hash => crossing.ask(next, others),
            _ => vec![next],
        }
    }

    /// The bytes asked over the link and not yet staged.
    fn in_flight(&self) -> u64 {
        self.crossings.values().map(Crossing::in_flight).sum()
    }

    /// Forgets file `key` of quest `id`, once whole or no longer waited
    /// on, so what it had asked no longer holds the link's budget.
    pub fn done(&mut self, id: &str, key: &str) {
        self.crossings.remove(&(id.to_owned(), key.to_owned()));
    }

    /// Forgets every crossing of quest `id`, no longer shared.
    pub fn forget(&mut self, id: &str) {
        self.crossings.retain(|(quest, _), _| quest != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUEST: &str = "QEST-20261013-4UHC5Q";
    const KEY: &str = "record/harness.log";
    const OTHER: &str = "record/JOURNEY.md";

    /// The piece at `at`, of `piece` bytes, of a file `len` long.
    fn landed(at: u64, piece: u64, len: u64) -> Landed {
        Landed { at, piece, len }
    }

    #[test]
    fn eight_pieces_are_kept_asked_and_none_past_the_end() {
        let mut ahead = Ahead::default();
        let len = 12 * PIECE + 5;
        let first = ahead.asks(QUEST, KEY, "abc", landed(0, PIECE, len), PIECE);
        assert_eq!(
            first,
            (1..=8).map(|n| n * PIECE).collect::<Vec<_>>(),
            "the first piece asks the next eight at once"
        );
        let second = ahead.asks(QUEST, KEY, "abc", landed(PIECE, PIECE, len), 2 * PIECE);
        assert_eq!(second, [9 * PIECE], "each piece after asks one more");
        let mut rest = Vec::new();
        for n in 2..12 {
            let piece = landed(n * PIECE, PIECE, len);
            rest.extend(ahead.asks(QUEST, KEY, "abc", piece, (n + 1) * PIECE));
        }
        assert_eq!(
            rest,
            [10 * PIECE, 11 * PIECE, 12 * PIECE],
            "and nothing is asked from the file's end on"
        );
    }

    #[test]
    fn files_crossing_together_share_what_is_asked_ahead() {
        let mut ahead = Ahead::default();
        let len = 40 * PIECE;
        let first = ahead.asks(QUEST, KEY, "abc", landed(0, PIECE, len), PIECE);
        assert_eq!(first.len(), 8, "the first file takes the link's eight");
        let other = ahead.asks(QUEST, OTHER, "def", landed(0, PIECE, len), PIECE);
        assert_eq!(other, [PIECE], "a second keeps its one piece asked");
        ahead.done(QUEST, KEY);
        let next = ahead.asks(QUEST, OTHER, "def", landed(PIECE, PIECE, len), 2 * PIECE);
        assert_eq!(next.len(), 8, "and takes the eight once the first is whole");
    }

    #[test]
    fn an_older_senders_long_pieces_are_asked_one_at_a_time() {
        let mut ahead = Ahead::default();
        let old = 1 << 20;
        let len = 3 * old;
        let first = ahead.asks(QUEST, KEY, "abc", landed(0, old, len), old);
        assert_eq!(first, [old], "a piece as long as the budget asks one");
        let second = ahead.asks(QUEST, KEY, "abc", landed(old, old, len), 2 * old);
        assert_eq!(second, [2 * old], "and the next one as it lands");
    }

    #[test]
    fn a_piece_of_no_crossing_known_asks_only_its_next() {
        let mut ahead = Ahead::default();
        let len = 20 * PIECE;
        ahead.asks(QUEST, KEY, "abc", landed(0, PIECE, len), PIECE);
        let other = ahead.asks(QUEST, KEY, "def", landed(PIECE, PIECE, len), 2 * PIECE);
        assert_eq!(other, [2 * PIECE], "other content asks as one piece did");
        ahead.done(QUEST, KEY);
        let after = ahead.asks(QUEST, KEY, "abc", landed(PIECE, PIECE, len), 2 * PIECE);
        assert_eq!(after, [2 * PIECE], "and so does one whose crossing is done");
        let again = ahead.asks(QUEST, KEY, "abc", landed(0, PIECE, len), PIECE);
        assert_eq!(again.len(), 8, "a crossing begun again asks ahead anew");
    }
}
