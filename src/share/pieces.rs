//! A large file crossing in pieces: each piece staged on this machine as it
//! comes, until the file is whole.
//!
//! A file larger than a piece crosses a piece to a frame, each asked for
//! once the one before it is staged, so the link's messages pass between
//! them rather than waiting behind the whole file. The staged pieces are
//! kept in mycelium's own folder, apart for each machine they come from and
//! named by the file and the content it is to hold, so two crossings of
//! different content never mix, and a crossing a link left unfinished is
//! cleared when that machine links again.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The most of a file one frame carries, in bytes.
pub const PIECE: u64 = 1 << 20;

/// Where the pieces from one machine are staged.
#[derive(Debug, Clone)]
pub struct Pieces {
    folder: PathBuf,
}

/// What a piece staged leaves.
#[derive(Debug, PartialEq, Eq)]
pub enum Staged {
    /// More is to come, from this offset.
    More(u64),
    /// The file is whole, and this is what it holds.
    Whole(Vec<u8>),
    /// The piece doesn't follow what is staged, as when a crossing of the
    /// same content was begun again over it, so it is passed over.
    Astray,
}

impl Pieces {
    /// The pieces from machine `other`, kept in mycelium's folder `state`.
    #[must_use]
    pub fn from(state: &Path, other: &str) -> Self {
        Self {
            folder: state.join("pieces").join(other),
        }
    }

    /// Clears every crossing an earlier link left unfinished.
    pub fn clear(&self) -> io::Result<()> {
        match fs::remove_dir_all(&self.folder) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Stages `body`, the piece at `at` of file `key` of quest `id`, which
    /// is to hold `len` bytes whose hash is `hash`.
    pub fn stage(
        &self,
        id: &str,
        key: &str,
        hash: &str,
        at: u64,
        len: u64,
        body: &[u8],
    ) -> io::Result<Staged> {
        fs::create_dir_all(&self.folder)?;
        let path = self.folder.join(format!(
            "{}-{hash}",
            blake3::hash(format!("{id}/{key}").as_bytes()).to_hex()
        ));
        let mut file = if at == 0 {
            fs::File::create(&path)?
        } else {
            match fs::OpenOptions::new().append(true).open(&path) {
                Ok(file) if file.metadata()?.len() == at => file,
                Ok(_) => return Ok(Staged::Astray),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(Staged::Astray);
                }
                Err(error) => return Err(error),
            }
        };
        file.write_all(body)?;
        drop(file);
        let next = at + u64::try_from(body.len()).unwrap_or(u64::MAX);
        if next < len && !body.is_empty() {
            return Ok(Staged::More(next));
        }
        let whole = fs::read(&path)?;
        fs::remove_file(&path)?;
        Ok(Staged::Whole(whole))
    }
}

/// The piece of `bytes` at `at`, and none past their end.
#[must_use]
pub fn piece_of(bytes: &[u8], at: u64) -> &[u8] {
    let start = usize::try_from(at).unwrap_or(usize::MAX).min(bytes.len());
    let end = start
        .saturating_add(usize::try_from(PIECE).unwrap_or(usize::MAX))
        .min(bytes.len());
    &bytes[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUEST: &str = "QEST-20261013-4UHC5Q";
    const KEY: &str = "scratch/bench/run.bin";

    #[test]
    fn pieces_staged_in_turn_make_the_file_whole() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let pieces = Pieces::from(dir.path(), "equilibrius");
        let bytes: Vec<u8> = (0..=u8::MAX).cycle().take(2 * 1024 * 1024 + 7).collect();
        let len = u64::try_from(bytes.len()).expect("a length");
        let mut at = 0;
        let whole = loop {
            match pieces
                .stage(QUEST, KEY, "abc", at, len, piece_of(&bytes, at))
                .expect("staged")
            {
                Staged::More(next) => at = next,
                Staged::Whole(whole) => break whole,
                Staged::Astray => panic!("a piece in turn went astray at {at}"),
            }
        };
        assert_eq!(at, 2 * PIECE, "three pieces, the last one short");
        assert!(whole == bytes, "the file is whole as it was sent");
        assert_eq!(
            fs::read_dir(dir.path().join("pieces/equilibrius"))
                .expect("the folder")
                .count(),
            0,
            "and nothing is left staged"
        );
    }

    #[test]
    fn a_piece_out_of_turn_is_passed_over() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let pieces = Pieces::from(dir.path(), "equilibrius");
        let piece = vec![1; usize::try_from(PIECE).expect("a size")];
        assert_eq!(
            pieces
                .stage(QUEST, KEY, "abc", PIECE, 3 * PIECE, &piece)
                .expect("staged"),
            Staged::Astray,
            "nothing begun, so a later piece has nothing to follow"
        );
        pieces
            .stage(QUEST, KEY, "abc", 0, 3 * PIECE, &piece)
            .expect("staged");
        assert_eq!(
            pieces
                .stage(QUEST, KEY, "abc", 2 * PIECE, 3 * PIECE, &piece)
                .expect("staged"),
            Staged::Astray,
            "a piece skipping one is passed over"
        );
        pieces.clear().expect("cleared");
        assert!(
            !dir.path().join("pieces/equilibrius").exists(),
            "a link's start clears what an earlier one left"
        );
    }
}
