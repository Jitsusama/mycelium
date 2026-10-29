//! The harness mesh's records read as the node's: a message file as a
//! [`Message`], a presence as a [`Session`]. The mesh's own fields are
//! read as they are, so sessions keep writing them unchanged.

use std::hash::{BuildHasher, RandomState};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::group::Group;
use crate::record::{Intent, Message, Session, State, Want, Window};
use crate::store::Presence;
use crate::wire::Draft;

/// The mesh's names for its machine-wide group. What is said there, such as
/// a hold before a reboot, means nothing on another machine.
const MACHINE_TARGETS: [&str; 3] = ["#machine", "#all", "#global"];

/// Whether a mesh message is its machine's alone: sent to the machine-wide
/// group, or answering a message that was, as `earlier` finds it.
pub fn stays_here(body: &Value, earlier: impl Fn(&str) -> Option<Value>) -> bool {
    let to_machine = |body: &Value| {
        body.get("to").and_then(Value::as_array).is_some_and(|to| {
            to.iter()
                .filter_map(Value::as_str)
                .any(|to| MACHINE_TARGETS.contains(&to))
        })
    };
    to_machine(body)
        || body
            .get("re")
            .and_then(Value::as_str)
            .and_then(earlier)
            .is_some_and(|answered| to_machine(&answered))
}

/// The leases of `session`, a session of this machine's, that another
/// machine can weigh: its change leases and its hands', each a file named by
/// its repository and its path there, as `core.nix:flake.nix`, so a session
/// in another clone of it overlaps. A change named by an absolute path, as a
/// file outside any checkout or a whole checkout rewritten is, names this
/// machine's disk, and where a checkout lies here is no matter there, so
/// neither crosses. Holdings carried from another machine never go on.
pub fn carried_leases(holdings: &[Value], session: &str) -> Vec<Value> {
    holdings
        .iter()
        .filter(|holding| holding.get("machine").is_none())
        .filter(|holding| {
            holding
                .get("handOf")
                .or_else(|| holding.get("session"))
                .and_then(Value::as_str)
                == Some(session)
        })
        .filter_map(|holding| holding.get("leases").and_then(Value::as_array))
        .flatten()
        .filter(|lease| {
            lease.get("kind").and_then(Value::as_str) == Some("change")
                && lease
                    .get("subject")
                    .and_then(Value::as_str)
                    .is_some_and(|subject| !subject.is_empty() && !subject.starts_with('/'))
        })
        .map(|lease| {
            let mut lease = lease.clone();
            if let Some(fields) = lease.as_object_mut() {
                fields.remove("checkout");
            }
            lease
        })
        .collect()
}

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
        _ if presence.unreachable_since.is_some() => State::Unreachable,
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
        quest: said
            .quest
            .or_else(|| said.owns.into_iter().find(|owned| owned.starts_with(QUEST))),
        wants: said.wants,
        window: said
            .terminal
            .filter(|_| presence.elsewhere.is_none())
            .map(|terminal| Window {
                terminal: terminal.driver_id,
                id: terminal.value,
                within: terminal.scope,
            }),
        at: said.at,
        presence: presence.body.clone(),
    })
}

/// How a quest's id begins, which an older presence names among what it
/// owns, before presences said their quest.
const QUEST: &str = "QEST-";

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
    quest: Option<String>,
    #[serde(default)]
    owns: Vec<String>,
    wants: Option<Want>,
    terminal: Option<Terminal>,
    at: String,
}

/// A session's window, as the harness's terminal driver names it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Terminal {
    driver_id: String,
    value: String,
    scope: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_changes_to_a_repositorys_files_go_to_other_machines() {
        let change = |subject: &str| {
            json!({"kind": "change", "subject": subject, "since": "s", "until": "u",
                   "checkout": "/home/jitsusama/src/core.nix"})
        };
        let holdings = [
            json!({"session": "01a1", "leases": [
                change("core.nix:flake.nix"),
                change("/home/jitsusama/notes.md"),
                change("/home/jitsusama/src/core.nix"),
                {"kind": "heavy", "subject": "build", "since": "s", "until": "u"},
            ]}),
            json!({"session": "01a1-hand", "handOf": "01a1",
                   "leases": [change("mycelium:src/mesh.rs")]}),
            json!({"session": "01b2", "leases": [change("core.nix:README.md")]}),
            json!({"session": "01a1", "machine": "equilibrius",
                   "leases": [change("core.nix:carried.nix")]}),
        ];
        let subjects: Vec<Value> = carried_leases(&holdings, "01a1")
            .iter()
            .map(|lease| lease["subject"].clone())
            .collect();
        assert_eq!(
            subjects,
            [json!("core.nix:flake.nix"), json!("mycelium:src/mesh.rs")],
            "its changes and its hand's to a repository's files, not another session's, \
             one carried here, an absolute path or heavy work"
        );
        assert!(
            carried_leases(&holdings, "01a1")
                .iter()
                .all(|lease| lease.get("checkout").is_none()),
            "and where the checkout lies here stays here"
        );
    }

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
    fn what_is_said_to_the_machine_stays_on_it_with_its_answers() {
        let hold = json!({"id": "1-16V6F4-1-0", "to": ["#all"], "intent": "hold"});
        let earlier = |id: &str| (id == "1-16V6F4-1-0").then(|| hold.clone());
        assert!(stays_here(&hold, earlier), "a hold to #all stays");
        assert!(
            stays_here(&json!({"to": ["@WCJQSG", "#machine"]}), earlier),
            "so does one to #machine"
        );
        let answer = json!({"to": ["@16V6F4"], "re": "1-16V6F4-1-0"});
        assert!(
            stays_here(&answer, earlier),
            "an answer to the hold stays with it"
        );
        let other = json!({"to": ["@16V6F4"], "re": "2-WCJQSG-1-0"});
        assert!(
            !stays_here(&other, earlier),
            "an answer to anything else crosses"
        );
        assert!(
            !stays_here(&json!({"to": ["#quest/4uhc5q"]}), earlier),
            "a quest's group crosses"
        );
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
    fn a_nodes_finding_is_written_as_the_mesh_reads_it() {
        let draft = Draft {
            from: "mycelium@optimus".to_owned(),
            groups: vec![Group::new("quest.wcjqsg.4uhc5q").expect("a group")],
            intent: Some(Intent::Finding),
            text: "kept beside".to_owned(),
            re: None,
        };
        let (id, body) = written(&draft, 4242);
        crate::store::folders::safe(&id).expect("its id is a name the store keeps");
        assert!(
            id.contains("-mycelium@optimus-4242-"),
            "its id names its sender and the node's pid: {id}"
        );
        assert_eq!(
            body["to"],
            json!(["#quest/wcjqsg/4uhc5q"]),
            "it goes to the quest's whole group"
        );
        assert_eq!(body["intent"], json!("finding"));
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
            unreachable_since: None,
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
        let out_of_reach = Presence {
            unreachable_since: Some(1_793_046_604_647),
            ..presence(true, Some("equilibrius"))
        };
        assert_eq!(
            session(&out_of_reach, "optimus")
                .expect("a presence reads")
                .state,
            State::Unreachable,
            "a session whose machine is out of reach reads as unreachable"
        );
    }

    #[test]
    fn a_session_says_its_quest_and_wants_and_here_its_window() {
        let presence = |body: Value, elsewhere: Option<&str>| Presence {
            session: "01a1".to_owned(),
            body,
            elsewhere: elsewhere.map(str::to_owned),
            lives: true,
            unreachable_since: None,
        };
        let said = json!({
            "session": "01a1", "addresses": ["4UHC5Q"], "at": "2026-10-28T21:50:00Z",
            "quest": "QEST-20261013-4UHC5Q", "owns": ["QEST-20261007-WCJQSG", "mycelium"],
            "wants": {"kind": "question", "since": "2026-10-28T21:49:00Z", "said": "which?"},
            "terminal": {"driverId": "kitty", "kind": "kitty-window", "hostId": "optimus",
                "value": "13", "scope": "unix:@kitty-6753"},
        });
        let here = session(&presence(said.clone(), None), "optimus").expect("a presence reads");
        assert_eq!(
            here.quest.as_deref(),
            Some("QEST-20261013-4UHC5Q"),
            "the quest it says it works on, over one it owns"
        );
        assert_eq!(
            here.wants.map(|wants| wants.kind).as_deref(),
            Some("question")
        );
        assert_eq!(
            here.window,
            Some(Window {
                terminal: "kitty".to_owned(),
                id: "13".to_owned(),
                within: Some("unix:@kitty-6753".to_owned()),
            }),
            "a session here says the window it runs in"
        );
        assert_eq!(
            here.presence["owns"],
            json!(["QEST-20261007-WCJQSG", "mycelium"]),
            "its presence goes along whole, for a reader that shows more of it"
        );
        let there =
            session(&presence(said, Some("equilibrius")), "optimus").expect("a presence reads");
        assert_eq!(
            there.window, None,
            "a window on another machine can't be gone to from this one"
        );
        let older = json!({
            "session": "01a1", "addresses": ["4UHC5Q"], "at": "2026-10-28T21:50:00Z",
            "owns": ["mycelium", "QEST-20261013-4UHC5Q"],
        });
        assert_eq!(
            session(&presence(older, None), "optimus")
                .expect("a presence reads")
                .quest
                .as_deref(),
            Some("QEST-20261013-4UHC5Q"),
            "an older presence's quest is the one it owns"
        );
    }
}
