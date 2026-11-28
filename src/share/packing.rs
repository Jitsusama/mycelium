//! How a piece of a large file is packed as it crosses. On a link whose
//! bytes, not its round trips, are what a file waits on, as a tunnel of
//! 26 KB/s was on 2026-11-27, a session's log deflates to a fraction of
//! itself, so it crosses in that fraction of the time.
//!
//! Only an end that says in its hello that it takes packed pieces is sent
//! one, so an end older than this is still sent every piece plain. A piece
//! that deflates to no less than itself crosses plain too.

use std::io;

use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec_with_limit;
use serde::{Deserialize, Serialize};

use super::pieces::PIECE;

/// How a piece's body was packed before it was put in base64.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Packing {
    /// Raw deflate, as RFC 1951 has it.
    Deflate,
}

/// How hard a piece is deflated: miniz's own default, which takes most of
/// what the best would on text, in a fraction of its time.
const LEVEL: u8 = 6;

/// The piece deflated, or `None` when that saves nothing, so it crosses
/// plain.
#[must_use]
pub fn pack(piece: &[u8]) -> Option<Vec<u8>> {
    let packed = compress_to_vec(piece, LEVEL);
    (packed.len() < piece.len()).then_some(packed)
}

/// A piece's body as it was before `packing`. A packed piece comes only
/// from an end as new as this, whose pieces are never longer than
/// [`PIECE`], so one that inflates past that is refused rather than let
/// fill this end's memory.
///
/// # Errors
///
/// When the body doesn't inflate, or inflates past one piece.
pub fn unpack(body: Vec<u8>, packing: Option<Packing>) -> io::Result<Vec<u8>> {
    let most = usize::try_from(PIECE).unwrap_or(usize::MAX);
    match packing {
        None => Ok(body),
        Some(Packing::Deflate) => decompress_to_vec_with_limit(&body, most).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a packed piece doesn't inflate: {:?}", error.status),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_of_a_log_packs_smaller_and_unpacks_to_itself() {
        let line = "{\"type\":\"message\",\"role\":\"assistant\",\"text\":\"kestrel\"}\n";
        let piece = line.repeat(1000).into_bytes();
        let packed = pack(&piece).expect("a log packs");
        assert!(
            packed.len() * 4 < piece.len(),
            "it packs well: {}",
            packed.len()
        );
        let unpacked = unpack(packed, Some(Packing::Deflate)).expect("it unpacks");
        assert_eq!(unpacked, piece);
    }

    #[test]
    fn a_piece_that_packs_no_smaller_crosses_plain() {
        // Bytes of no pattern, as a piece of an archive already deflated.
        let mut seed = 0x2545_f491_u32;
        let piece: Vec<u8> = (0..4096)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed.to_le_bytes()[0]
            })
            .collect();
        assert_eq!(pack(&piece), None);
    }

    #[test]
    fn a_plain_body_is_kept_as_it_came() {
        let body = b"# Tern\n".to_vec();
        assert_eq!(unpack(body.clone(), None).expect("plain"), body);
    }

    #[test]
    fn a_packed_piece_that_inflates_past_one_piece_is_refused() {
        let most = usize::try_from(PIECE).expect("a size");
        let packed = compress_to_vec(&vec![0; most + 1], LEVEL);
        let error = unpack(packed, Some(Packing::Deflate)).expect_err("too long");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_packed_body_that_isnt_deflate_is_refused() {
        let error = unpack(vec![0xff; 16], Some(Packing::Deflate)).expect_err("not deflate");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
