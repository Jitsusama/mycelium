//! Realm flows: the one-way roads a realm's messages may take into another
//! realm, as a machine's configuration declares them.
//!
//! Realms are kept apart: nothing of one reaches another. A flow opens one
//! road, such as personal to work, and never its reverse. Only text
//! crosses it, to peers by address, and only when its sender names the
//! realm it is for. The realm a message leaves hands it across only under
//! a flow to the other, and the realm it reaches takes it only under a
//! flow from the first, so a fault at one end can't open the other way.
//! Both ends keep every crossing, taken or refused, as a line of their
//! crossings log.

use std::collections::{BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::group::Group;
use crate::record::{Intent, Message};
use crate::wire::{Answer, Request};

/// How long the other realm's node on this machine has to answer a
/// crossing: it writes one file, so this is spent only on one that hangs.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(3);

/// The flows a realm's node on this machine keeps.
#[derive(Debug, Clone, Default)]
pub struct Flows {
    /// The realms this one's messages may go to.
    pub to: BTreeSet<String>,
    /// The realms whose messages this one takes.
    pub from: BTreeSet<String>,
    /// The folder each realm's node on this machine serves its socket in,
    /// as `<realm>.sock`. A realm whose socket isn't there has no node on
    /// this machine, so a message for it waits for a link to carry it to a
    /// machine that has.
    pub sockets: PathBuf,
    /// The crossings log, a JSON line each.
    pub log: PathBuf,
}

impl Flows {
    /// The socket `realm`'s node on this machine serves.
    #[must_use]
    pub fn socket(&self, realm: &str) -> PathBuf {
        self.sockets.join(format!("{realm}.sock"))
    }
}

/// Why a crossing was refused, in words the sender's harness tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// No flow is declared that way.
    NoFlow,
    /// It isn't a fyi, finding or question to peers by address.
    NotText,
    /// No session of the other realm hears it.
    Unheard,
    /// The other realm's node on this machine doesn't answer.
    Unreached,
}

/// A crossing refused, and why, in one line for its sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// Which kind of refusal.
    pub code: Code,
    /// Why, for the sender.
    pub why: String,
}

impl Refusal {
    /// A refusal of `code`, for `why`.
    #[must_use]
    pub fn new(code: Code, why: impl Into<String>) -> Self {
        Self {
            code,
            why: why.into(),
        }
    }
}

/// A message one realm's node hands another's on the same machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crossing {
    /// The message, as its sender's node wrote it.
    pub message: Message,
    /// The realm it comes from.
    pub from_realm: String,
}

/// Whether a message intended `intent` to `groups` may cross: only a fyi,
/// a finding or a question, since a hold or a pivot changes what other
/// sessions do, and only to peers by address, never to a group such as a
/// quest's or the machine's.
///
/// # Errors
///
/// A [`Code::NotText`] refusal saying which it is not.
pub fn text_only(intent: Option<Intent>, groups: &[Group]) -> Result<(), Refusal> {
    match intent {
        Some(Intent::Fyi | Intent::Finding | Intent::Question) => {}
        Some(other) => {
            return Err(Refusal::new(
                Code::NotText,
                format!(
                    "a {} changes what other sessions do, so it stays in its realm; only a fyi, \
                     finding or question crosses",
                    serde_json::to_value(other)
                        .ok()
                        .and_then(|said| said.as_str().map(str::to_owned))
                        .unwrap_or_default()
                ),
            ));
        }
        None => {
            return Err(Refusal::new(
                Code::NotText,
                "a message with no intent doesn't cross; say it is a fyi, finding or question",
            ));
        }
    }
    if groups.is_empty() {
        return Err(Refusal::new(Code::NotText, "it goes to no one"));
    }
    if let Some(group) = groups.iter().find(|group| address(group).is_none()) {
        return Err(Refusal::new(
            Code::NotText,
            format!("{group} is a group, and only a message to peers by address crosses"),
        ));
    }
    Ok(())
}

/// The address a peer's own group names, such as `4uhc5q` of
/// `peer.4uhc5q`, or none for any other group.
#[must_use]
pub fn address(group: &Group) -> Option<&str> {
    group
        .as_str()
        .strip_prefix("peer.")
        .filter(|address| !address.is_empty() && !address.contains('.'))
}

/// One line of a crossings log: a crossing taken, or refused and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    /// When it was judged, as an RFC 3339 instant.
    pub at: String,
    /// The realm it came from.
    pub from_realm: String,
    /// The realm it was for.
    pub to_realm: String,
    /// Who sent it.
    pub from: String,
    /// The groups it went to.
    pub to: Vec<Group>,
    /// What it asked of those who hear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
    /// Its id, the same in both realms.
    pub id: String,
    /// Whether it was taken.
    pub taken: bool,
    /// Why it wasn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<Refusal>,
}

impl Kept {
    /// The line for `message`, from `from_realm` into `to_realm`, taken
    /// or refused as `verdict` says.
    #[must_use]
    pub fn of(
        message: &Message,
        from_realm: &str,
        to_realm: &str,
        verdict: Result<(), &Refusal>,
    ) -> Self {
        Self {
            at: jiff::Timestamp::now().to_string(),
            from_realm: from_realm.to_owned(),
            to_realm: to_realm.to_owned(),
            from: message.from.clone(),
            to: message.groups.clone(),
            intent: message.intent,
            id: message.id.clone(),
            taken: verdict.is_ok(),
            refused: verdict.err().cloned(),
        }
    }
}

/// Keeps `line` at the end of the log at `log`, in one write, so lines
/// kept at once never interleave.
///
/// # Errors
///
/// When the log can't be opened or written.
pub fn keep(log: &Path, line: &Kept) -> io::Result<()> {
    if let Some(folder) = log.parent() {
        fs::create_dir_all(folder)?;
    }
    let mut text = serde_json::to_vec(line).map_err(io::Error::other)?;
    text.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?
        .write_all(&text)
}

/// The ids of the messages of `realm` the log at `log` says were judged
/// for good, taken or refused, so a message waiting to cross isn't handed
/// across again after the node restarts. One refused as unreached is
/// still to be tried.
#[must_use]
pub fn settled(log: &Path, realm: &str) -> HashSet<String> {
    let Ok(file) = fs::File::open(log) else {
        return HashSet::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Kept>(&line).ok())
        .filter(|kept| kept.from_realm == realm)
        .filter(|kept| {
            kept.refused
                .as_ref()
                .is_none_or(|refused| refused.code != Code::Unreached)
        })
        .map(|kept| kept.id)
        .collect()
}

/// Hands `crossing` to the node serving `socket` and reads its answer. A
/// node that can't be reached, or doesn't answer in time, is a
/// [`Code::Unreached`] refusal.
pub async fn hand(socket: &Path, crossing: Crossing) -> Answer {
    let unreached = |why: String| Answer::Refused(Refusal::new(Code::Unreached, why));
    let asked = async {
        let stream = tokio::net::UnixStream::connect(socket).await?;
        let (reading, mut writing) = stream.into_split();
        let mut line = serde_json::to_vec(&Request::Cross(crossing)).map_err(io::Error::other)?;
        line.push(b'\n');
        writing.write_all(&line).await?;
        let mut answer = String::new();
        tokio::io::BufReader::new(reading)
            .read_line(&mut answer)
            .await?;
        serde_json::from_str::<Answer>(&answer).map_err(io::Error::other)
    };
    match tokio::time::timeout(ANSWER_WITHIN, asked).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(error)) => unreached(format!("the other realm's node didn't answer: {error}")),
        Err(_) => unreached(format!(
            "the other realm's node didn't answer within {} s",
            ANSWER_WITHIN.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str) -> Group {
        Group::new(name).expect("a valid group")
    }

    fn message(id: &str) -> Message {
        Message {
            id: id.to_owned(),
            at: "2026-11-03T21:00:00Z".to_owned(),
            from: "Q9METH".to_owned(),
            groups: vec![group("peer.16v6f4")],
            intent: Some(Intent::Finding),
            text: "a finding".to_owned(),
            re: None,
            supersedes: None,
            from_realm: None,
            for_realm: None,
        }
    }

    #[test]
    fn only_a_fyi_finding_or_question_to_peers_by_address_is_text() {
        let peers = [group("peer.16v6f4"), group("peer.4uhc5q")];
        for intent in [Intent::Fyi, Intent::Finding, Intent::Question] {
            assert_eq!(
                text_only(Some(intent), &peers),
                Ok(()),
                "{intent:?} crosses"
            );
        }
        for intent in [Some(Intent::Hold), Some(Intent::Pivot), None] {
            let refused = text_only(intent, &peers).expect_err("it stays");
            assert_eq!(refused.code, Code::NotText, "{intent:?} stays");
        }
        for groups in [
            vec![group("machine")],
            vec![group("all")],
            vec![group("peer.16v6f4"), group("quest.wcjqsg")],
            vec![group("peer.wcjqsg.more")],
            Vec::new(),
        ] {
            let refused = text_only(Some(Intent::Fyi), &groups).expect_err("it stays");
            assert_eq!(refused.code, Code::NotText, "{groups:?} stays");
        }
    }

    #[test]
    fn a_crossing_judged_for_good_is_settled_and_one_unreached_is_not() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let log = dir.path().join("state").join("crossings.jsonl");
        let unreached = Refusal::new(Code::Unreached, "not running");
        let unheard = Refusal::new(Code::Unheard, "no one");
        keep(
            &log,
            &Kept::of(&message("taken"), "personal", "work", Ok(())),
        )
        .expect("kept");
        keep(
            &log,
            &Kept::of(&message("unreached"), "personal", "work", Err(&unreached)),
        )
        .expect("kept");
        keep(
            &log,
            &Kept::of(&message("unheard"), "personal", "work", Err(&unheard)),
        )
        .expect("kept");
        keep(&log, &Kept::of(&message("theirs"), "other", "work", Ok(()))).expect("kept");

        let settled = settled(&log, "personal");
        assert_eq!(
            settled,
            HashSet::from(["taken".to_owned(), "unheard".to_owned()])
        );
        let text = fs::read_to_string(&log).expect("the log reads");
        let first: serde_json::Value =
            serde_json::from_str(text.lines().next().expect("a line")).expect("a JSON line");
        assert_eq!(first["from_realm"], "personal");
        assert_eq!(first["to_realm"], "work");
        assert_eq!(first["to"], serde_json::json!(["peer.16v6f4"]));
        assert_eq!(first["intent"], "finding");
        assert_eq!(first["taken"], true);
    }

    #[tokio::test]
    async fn a_node_that_isnt_there_is_unreached() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let crossing = Crossing {
            message: message("one"),
            from_realm: "personal".to_owned(),
        };
        let Answer::Refused(refused) = hand(&dir.path().join("work.sock"), crossing).await else {
            panic!("a node that isn't there doesn't take it");
        };
        assert_eq!(refused.code, Code::Unreached);
    }
}
