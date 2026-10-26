//! The harness mesh's records read as the node's: a message file as a
//! [`Message`], a presence as a [`Session`]. The mesh's own fields are
//! read as they are, so sessions keep writing them unchanged.

use std::hash::{BuildHasher, RandomState};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::group::Group;
use crate::record::{Intent, Message, Session, State};
use crate::store::Presence;
use crate::wire::Draft;

/// The group a mesh target names: `@4UHC5Q` is the peer's own group
/// `peer.4uhc5q`, and `#quest/4uhc5q` is `quest.4uhc5q`.
pub fn group_for(target: &str) -> Option<Group> {
    let name = if let Some(address) = target.strip_prefix('@') {
        format!("peer.{address}")
    } else {
        target.strip_prefix('#')?.replace('/', ".")
    };
    Group::new(name.to_lowercase()).ok()
}

/// The mesh target a group is written as: the peer's own group
/// `peer.4uhc5q` is `@4UHC5Q`, and `quest.4uhc5q` is `#quest/4uhc5q`.
pub fn target_for(group: &Group) -> String {
    match group.as_str().strip_prefix("peer.") {
        Some(address) if !address.contains('.') => format!("@{}", address.to_uppercase()),
        _ => format!("#{}", group.as_str().replace('.', "/")),
    }
}

/// A draft written as a mesh message, sent now by process `pid`: its id,
/// and its file's contents. The id is the mesh's own shape, the time first
/// so ids sort as they were sent, and a random tail keeps two apart.
pub fn written(draft: &Draft, pid: u32) -> (String, Value) {
    let now = jiff::Timestamp::now();
    // Each `RandomState` is seeded afresh, so its hash of nothing is a
    // random number without a dependency to draw one.
    let tail = RandomState::new().hash_one(()) & 0xffff_ffff;
    let id = format!("{}-{}-{pid}-{tail:08x}", now.as_millisecond(), draft.from);
    let mut body = json!({
        "v": 1,
        "id": id,
        "at": now.to_string(),
        "from": draft.from,
        "to": draft.groups.iter().map(target_for).collect::<Vec<_>>(),
        "text": draft.text,
        "pid": pid,
    });
    if let Some(intent) = draft.intent {
        body["intent"] = json!(intent);
    }
    if let Some(re) = &draft.re {
        body["re"] = json!(re);
    }
    (id, body)
}

/// The message a mesh message's file holds, if it goes to any group.
pub fn message(body: &Value) -> Option<Message> {
    let sent = Sent::deserialize(body).ok()?;
    let groups: Vec<Group> = sent.to.iter().filter_map(|to| group_for(to)).collect();
    if groups.is_empty() {
        return None;
    }
    Some(Message {
        id: sent.id,
        at: sent.at,
        from: sent.from,
        groups,
        intent: sent.intent,
        text: sent.text,
        re: sent.re,
        supersedes: sent.supersedes,
    })
}

/// The session a presence describes, on `here` unless it runs elsewhere.
pub fn session(presence: &Presence, here: &str) -> Option<Session> {
    let said = Said::deserialize(&presence.body).ok()?;
    let state = match (presence.lives, said.busy, said.state.as_deref()) {
        (false, _, _) => State::Ended,
        (true, true, _) | (true, _, Some("busy")) => State::Busy,
        (true, _, _) => State::Idle,
    };
    Some(Session {
        id: said.session,
        address: said.addresses.into_iter().next()?,
        machine: presence
            .elsewhere
            .clone()
            .unwrap_or_else(|| here.to_owned()),
        title: said.title,
        working_on: said.works_on,
        groups: said
            .groups
            .iter()
            .filter_map(|group| group_for(group))
            .collect(),
        state,
        step: said.step,
        since: said.since,
        orchestrated_by: said.orchestrated_by,
        at: said.at,
    })
}

#[derive(Deserialize)]
struct Sent {
    id: String,
    at: String,
    from: String,
    to: Vec<String>,
    intent: Option<Intent>,
    text: String,
    re: Option<String>,
    supersedes: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Said {
    session: String,
    addresses: Vec<String>,
    #[serde(default)]
    groups: Vec<String>,
    works_on: Option<String>,
    #[serde(default)]
    busy: bool,
    state: Option<String>,
    since: Option<String>,
    step: Option<String>,
    title: Option<String>,
    orchestrated_by: Option<String>,
    at: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn targets_name_groups() {
        let named = |target| group_for(target).map(String::from);
        assert_eq!(named("@4UHC5Q").as_deref(), Some("peer.4uhc5q"));
        assert_eq!(
            named("#quest/wcjqsg/2cvw1s").as_deref(),
            Some("quest.wcjqsg.2cvw1s")
        );
        assert_eq!(named("#machine").as_deref(), Some("machine"));
        assert_eq!(
            named("#repo/agentic-harness-pi").as_deref(),
            Some("repo.agentic-harness-pi")
        );
        assert_eq!(named("orchestrator"), None);
    }

    #[test]
    fn groups_are_written_back_as_targets() {
        for target in ["@4UHC5Q", "#quest/wcjqsg/2cvw1s", "#machine"] {
            let group = group_for(target).expect("the test names a valid target");
            assert_eq!(target_for(&group), target);
        }
    }

    #[test]
    fn a_draft_is_written_as_the_mesh_writes_a_message() {
        let draft = Draft {
            from: "joel".to_owned(),
            groups: vec![group_for("@4UHC5Q").expect("a valid target")],
            intent: Some(Intent::Question),
            text: "how far along?".to_owned(),
            re: None,
        };
        let (id, body) = written(&draft, 42);
        assert!(id.contains("-joel-42-"), "the id is the mesh's shape: {id}");
        let read = message(&body).expect("what is written reads back");
        assert_eq!(
            (read.id, read.text, read.intent),
            (id, draft.text, draft.intent)
        );
        assert_eq!(body["to"], json!(["@4UHC5Q"]));
    }

    #[test]
    fn a_mesh_message_reads_as_a_message() {
        let body = json!({
            "v": 1, "id": "1793046604647-0FX11G-875563-952d5879", "at": "2026-10-26T20:30:04.647Z",
            "from": "0FX11G", "to": ["@WCJQSG", "#machine"], "intent": "fyi", "text": "switched", "pid": 875_563,
        });
        let message = message(&body).expect("a mesh message reads");
        assert_eq!(
            message.groups.iter().map(Group::as_str).collect::<Vec<_>>(),
            ["peer.wcjqsg", "machine"]
        );
        assert_eq!(message.intent, Some(Intent::Fyi));
    }

    #[test]
    fn a_presence_reads_as_a_session_where_it_runs() {
        let presence = |lives, elsewhere: Option<&str>| Presence {
            session: "01a1".to_owned(),
            body: json!({
                "session": "01a1", "addresses": ["4UHC5Q", "7DD51C6F"], "groups": ["#quest/4uhc5q"],
                "busy": true, "state": "busy", "at": "2026-10-26T20:31:17.392Z",
            }),
            elsewhere: elsewhere.map(str::to_owned),
            lives,
        };
        let here = session(&presence(true, None), "optimus").expect("a presence reads");
        assert_eq!(
            (here.address.as_str(), here.machine.as_str(), here.state),
            ("4UHC5Q", "optimus", State::Busy)
        );
        let there =
            session(&presence(true, Some("equilibrius")), "optimus").expect("a presence reads");
        assert_eq!(there.machine, "equilibrius");
        let ended = session(&presence(false, None), "optimus").expect("a presence reads");
        assert_eq!(ended.state, State::Ended);
    }
}
