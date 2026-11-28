//! Whether this machine's lend is taken, as its file was last read, for
//! the node's health record. A lend that isn't taken refuses every machine
//! it lends to, and only the node's log said so, so a lend file the seal
//! refused went unseen until the machines it lent to were all refused.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use serde_json::{Value, json};

/// Why the lend's file wasn't taken when it was last read, or nothing
/// where it was, or hasn't been read yet.
#[derive(Default)]
pub(super) struct Taken(Mutex<Option<String>>);

impl Taken {
    /// Enters how the file read: taken, or not taken and why.
    pub(super) fn read(&self, why: Option<String>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = why;
    }

    /// The lend as the daemon interface's `health` names a source:
    /// answering, or refused with why, for `file`.
    pub(super) fn health(&self, file: &Path) -> Value {
        let path = file.display().to_string();
        match &*self.0.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(why) => json!({
                "name": "lend",
                "file": path,
                "state": "refused",
                "why": format!("this machine's lend isn't taken: {why}"),
            }),
            None => json!({"name": "lend", "file": path, "state": "answering"}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lend_not_taken_says_why_until_it_is_taken_again() {
        let taken = Taken::default();
        let file = Path::new("lend.json");
        assert_eq!(
            taken.health(file)["state"],
            "answering",
            "a lend not yet read is answering"
        );
        taken.read(Some("it isn't sealed".to_owned()));
        let health = taken.health(file);
        assert_eq!(health["state"], "refused", "one not taken is refused");
        assert_eq!(
            health["why"], "this machine's lend isn't taken: it isn't sealed",
            "saying why"
        );
        taken.read(None);
        assert_eq!(
            taken.health(file)["state"],
            "answering",
            "and one taken again answers"
        );
    }
}
