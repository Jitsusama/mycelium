//! Steers across a link. A steer the other end carries is written into
//! this machine's inbox for its session, and a talk it opens is followed
//! here: the whole lines its session appends are carried back, from where
//! the other end's copy ends, until a stage ends the talk. The other end
//! says which talks it still follows each time a link starts, so a talk
//! carries on across a dropped link.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::steer::{self, Steer, Talk};

/// The talks this end carries back, and how far each has been carried.
pub(super) struct Following {
    inboxes: PathBuf,
    /// Each talk's address and id, and the bytes of its file carried.
    talks: HashMap<(String, String), u64>,
}

/// Lines a session added to a talk, carried from where in its file they
/// start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replies {
    /// The session's address.
    pub address: String,
    /// The talk's id.
    pub id: String,
    /// Where in its file the lines start.
    pub from: u64,
    /// Whole lines, each with its newline.
    pub lines: String,
}

impl Following {
    /// Follows nothing yet, in the inboxes at `inboxes`.
    pub(super) fn new(inboxes: PathBuf) -> Self {
        Self {
            inboxes,
            talks: HashMap::new(),
        }
    }

    /// Writes what the other end says to `address` into its inbox here,
    /// following the talk it opens from the start.
    pub(super) fn deliver(&mut self, address: &str, said: &Value) -> Result<(), String> {
        let steer = Steer::new(address, said)?;
        steer
            .deliver(&self.inboxes)
            .map_err(|error| format!("its inbox refused it: {error}"))?;
        if let Some(talk) = steer.talk() {
            self.talks
                .insert((steer.address.clone(), talk.to_owned()), 0);
        }
        Ok(())
    }

    /// Carries on with the talks the other end still follows, from where
    /// its copies end, for each whose file is here; the rest are another
    /// machine's, or none.
    pub(super) fn follow(&mut self, talks: Vec<Talk>) {
        for talk in talks {
            let here = steer::replies(&self.inboxes, &talk.address, &talk.id);
            if here.is_some_and(|path| path.is_file()) {
                self.talks.insert((talk.address, talk.id), talk.has);
            }
        }
    }

    /// The lines each talk's session has appended since last looked, and
    /// no more of a talk once a stage has ended it, or once nothing has
    /// been written to it for [`steer::FOLLOWED_FOR`]. A file that can't be
    /// read is looked at again next time.
    pub(super) fn look(&mut self) -> Vec<Replies> {
        let mut carried = Vec::new();
        let inboxes = &self.inboxes;
        self.talks.retain(|(address, id), from| {
            let Some(path) = steer::replies(inboxes, address, id) else {
                return false;
            };
            let (lines, next) = match steer::lines_from(&path, *from) {
                Ok(read) => read,
                Err(error) => {
                    eprintln!("reading the talk {id} for {address}: {error}");
                    return true;
                }
            };
            if lines.is_empty() {
                return !stale(&path);
            }
            let ended = steer::ends(&lines);
            carried.push(Replies {
                address: address.clone(),
                id: id.clone(),
                from: *from,
                lines,
            });
            *from = next;
            !ended
        });
        carried
    }
}

/// Whether nothing has been written to `path` for [`steer::FOLLOWED_FOR`].
fn stale(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age >= steer::FOLLOWED_FOR)
}

/// Adds what the other end carried of a talk to its file here, if this
/// machine opened that talk; a talk this machine never opened isn't made.
pub(super) fn keep(inboxes: &std::path::Path, replies: &Replies) -> io::Result<()> {
    let Some(path) = steer::replies(inboxes, &replies.address, &replies.id) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} {} can't name a talk", replies.address, replies.id),
        ));
    };
    if !path.is_file() {
        return Ok(());
    }
    steer::append(&path, replies.from, &replies.lines)
}
