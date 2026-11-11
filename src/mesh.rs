//! The harness mesh's records read as the node's: a message file as a
//! [`Message`], a presence as a [`Session`]. The mesh's own fields are
//! read as they are, so sessions keep writing them unchanged.

use std::ffi::OsStr;
use std::hash::{BuildHasher, RandomState};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::group::Group;
use crate::hold::Held;
use crate::record::{Intent, Landing, Message, Session, State, Want, Window};
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
        from_realm: sent.from_realm,
        for_realm: sent.for_realm,
    })
}

/// The mesh file of a message that crossed from `from_realm` by a flow,
/// written by process `pid`: its text alone, under the id it was sent
/// with, marked with the realm it came from.
pub fn crossed(message: &Message, from_realm: &str, pid: u32) -> Value {
    let mut body = json!({
        "v": 1,
        "id": message.id,
        "at": message.at,
        "from": message.from,
        "to": message.groups.iter().map(target_for).collect::<Vec<_>>(),
        "text": message.text,
        "pid": pid,
        "fromRealm": from_realm,
    });
    if let Some(intent) = message.intent {
        body["intent"] = json!(intent);
    }
    if let Some(re) = &message.re {
        body["re"] = json!(re);
    }
    body
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
        landings: landings(&said.landings),
        window: said
            .terminal
            .filter(|_| presence.elsewhere.is_none())
            .map(|terminal| Window {
                terminal: terminal.driver_id,
                id: terminal.value,
                within: terminal.scope,
                app_id: None,
            }),
        // Its own machine's holders are read where they run; a link writes
        // the id one keeps a session elsewhere by into its presence.
        held: presence
            .elsewhere
            .as_ref()
            .and_then(|_| presence.body.get(HELD_AS)?.as_str())
            .map(str::to_owned),
        attach: None,
        host: said.hosted["host"].as_str().map(str::to_owned),
        conversation: said.hosted["conversation"].as_str().map(str::to_owned),
        at: said.at,
        presence: presence.body.clone(),
    })
}

/// The field of a presence carried from another machine that a link writes
/// the session's held id into, as that machine's holder keeps it.
pub const HELD_AS: &str = "heldAs";

/// What is held here that holds this machine's session: the holder whose
/// program is the session's own process, as a session started with
/// `mycelium hold -- pi` is. A hosted session's process is its host's,
/// which many share, so the host's holder is none of theirs: attaching
/// would join the host's terminal, not the session.
///
/// A parked session's process has gone, and its holder's pid is 0 till
/// it wakes, so it's known by the wake file its holder gave it
/// (`hold::wake`), which the presence names as `wake`: its holder still
/// keeps the session's window, which the launcher keeps the place of
/// before it moves the session.
pub fn held_by<'a>(presence: &Presence, held: &'a [Held]) -> Option<&'a Held> {
    if presence.elsewhere.is_some() || presence.body.get("hosted").is_some_and(Value::is_object) {
        return None;
    }
    let pid = presence
        .body
        .get("owner")
        .and_then(|owner| owner.get("pid"))
        .and_then(Value::as_u64);
    let woken_by = presence
        .body
        .get("wake")
        .and_then(Value::as_str)
        .and_then(|wake| Path::new(wake).file_stem())
        .and_then(OsStr::to_str);
    held.iter()
        .find(|one| one.pid != 0 && pid == Some(u64::from(one.pid)))
        .or_else(|| held.iter().find(|one| woken_by == Some(one.id.as_str())))
}

/// What is held here that joins this machine's hosted session to a window:
/// the holder whose command names the session after `--session`, by its
/// id or its host's id for it, as `mycelium attach --host` does and as the
/// harness's join does once a pi has handed its window to a pi host. The
/// window stays where Joel had it, and this is how the session is known to
/// be in it. A join that fell back to plain pi names a log, not the id, so
/// it joins nothing: that pi is a session of its own.
pub fn joined_by<'a>(presence: &Presence, held: &'a [Held]) -> Option<&'a Held> {
    if presence.elsewhere.is_some() {
        return None;
    }
    let hosted = presence
        .body
        .get("hosted")
        .filter(|hosted| hosted.is_object())?;
    let conversation = hosted.get("conversation").and_then(Value::as_str);
    let names = |id: &str| id == presence.session || Some(id) == conversation;
    held.iter().find(|one| {
        one.command
            .windows(2)
            .any(|words| words[0] == "--session" && names(&words[1]))
    })
}

/// The words a window runs to view a hosted session, as its presence says
/// them: the harness's join, which attaches through the host while it
/// holds the conversation and carries on as plain pi on its log once the
/// host releases it, so a release keeps the window. None when it says no
/// words, or any that aren't, as an older harness's presence doesn't.
#[must_use]
pub fn viewed_by(presence: &Value) -> Option<Vec<String>> {
    let words = presence.get("hosted")?.get("view")?.as_array()?;
    let view: Option<Vec<String>> = words
        .iter()
        .map(|word| word.as_str().map(str::to_owned))
        .collect();
    view.filter(|view| !view.is_empty())
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
    #[serde(rename = "fromRealm")]
    from_realm: Option<String>,
    #[serde(rename = "forRealm")]
    for_realm: Option<String>,
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
    // Read one by one, so a landing it can't read costs only itself.
    #[serde(default)]
    landings: Value,
    terminal: Option<Terminal>,
    // The pi host it runs in among others, and its conversation there,
    // read field by field, so a part it can't read costs only that part.
    #[serde(default)]
    hosted: Value,
    at: String,
}

/// The landings a presence says, leaving out any it can't read.
fn landings(said: &Value) -> Vec<Landing> {
    said.as_array()
        .into_iter()
        .flatten()
        .filter_map(|landing| Landing::deserialize(landing).ok())
        .collect()
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
            realm: None,
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
            realm: None,
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
                app_id: None,
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

    #[test]
    fn a_session_says_what_it_landed_leaving_out_what_cant_be_read() {
        let presence = |landings: Value| Presence {
            session: "01a1".to_owned(),
            body: json!({
                "session": "01a1", "addresses": ["4UHC5Q"], "at": "2026-11-01T14:00:00Z",
                "landings": landings,
            }),
            elsewhere: Some("equilibrius".to_owned()),
            lives: true,
            unreachable_since: None,
        };
        let landed = json!({"at": "2026-11-01T13:58:00Z", "ref": "mycelium#82",
            "what": "Sessions say what they landed", "person": "less to read"});
        let read = session(
            &presence(
                json!([landed, {"ref": "no time or line"}, {"at": "2026-11-01T12:00:00Z",
                "ref": "PLAN-20261026-IZH11J#3", "what": "an item done"}]),
            ),
            "optimus",
        )
        .expect("a presence reads");
        assert_eq!(
            read.landings
                .iter()
                .map(|landing| landing.reference.as_str())
                .collect::<Vec<_>>(),
            ["mycelium#82", "PLAN-20261026-IZH11J#3"],
            "each landing it can read, in the order said, and none it can't"
        );
        assert_eq!(
            read.landings[0].person.as_deref(),
            Some("less to read"),
            "a landing says what it means to the person"
        );
        let unread = session(&presence(json!("not a list")), "optimus");
        assert!(
            unread.is_some_and(|session| session.landings.is_empty()),
            "landings it can't read never cost the session itself"
        );
    }

    #[test]
    fn a_session_here_says_what_holds_it() {
        let presence = |pid: u32, elsewhere: Option<&str>| Presence {
            session: "01a1".to_owned(),
            body: json!({"session": "01a1", "owner": {"pid": pid}}),
            elsewhere: elsewhere.map(str::to_owned),
            lives: true,
            unreachable_since: None,
        };
        let held = |id: &str, pid| Held {
            id: id.to_owned(),
            command: vec!["pi".to_owned()],
            folder: "/home/jitsusama".into(),
            holder: 1,
            pid,
            parked: false,
            since: "2026-10-28T23:00:00Z".to_owned(),
            windows: Vec::new(),
        };
        let here = [held("5b992038", 4242), held("0d1e2f3a", 77)];
        assert_eq!(
            held_by(&presence(4242, None), &here).map(|one| one.id.as_str()),
            Some("5b992038"),
            "the holder whose program is the session's process"
        );
        assert_eq!(
            held_by(&presence(4243, None), &here),
            None,
            "a session no holder runs isn't held"
        );
        assert_eq!(
            held_by(&presence(4242, Some("equilibrius")), &here),
            None,
            "a process elsewhere is never one held here"
        );
        let hosted = Presence {
            body: json!({
                "session": "01a1", "owner": {"pid": 4242},
                "hosted": {"host": "lantern", "conversation": "c-01a1"},
            }),
            ..presence(4242, None)
        };
        assert_eq!(
            held_by(&hosted, &here),
            None,
            "a hosted session's process is its host's, whose holder is none of its own"
        );
        let parked = Held {
            parked: true,
            windows: vec![Window {
                terminal: "kitty".to_owned(),
                id: "4".to_owned(),
                within: None,
                app_id: Some("pi-spawned.9".to_owned()),
            }],
            ..held("685a586d", 0)
        };
        let napping = Presence {
            body: json!({
                "session": "01a1", "owner": {"pid": 4244},
                "wake": "/run/user/1000/mycelium/work.held/685a586d.wake",
            }),
            ..presence(4244, None)
        };
        let here = [held("5b992038", 4242), parked];
        assert_eq!(
            held_by(&napping, &here).map(|one| one.id.as_str()),
            Some("685a586d"),
            "a parked session is held by the holder its wake file names"
        );
        assert_eq!(
            held_by(&presence(0, None), &here),
            None,
            "a parked holder's pid of 0 holds no session that names none"
        );
    }

    #[test]
    fn a_hosted_session_is_in_the_window_whose_holder_joins_it() {
        let hosted = |elsewhere: Option<&str>| Presence {
            session: "01a1".to_owned(),
            body: json!({
                "session": "01a1", "owner": {"pid": 4242},
                "hosted": {"host": "work-attended", "conversation": "c-01a1"},
            }),
            elsewhere: elsewhere.map(str::to_owned),
            lives: true,
            unreachable_since: None,
        };
        let held = |id: &str, command: &[&str]| Held {
            id: id.to_owned(),
            command: command.iter().map(|word| (*word).to_owned()).collect(),
            folder: "/home/jitsusama".into(),
            holder: 1,
            pid: 77,
            parked: false,
            since: "2026-11-11T02:00:00Z".to_owned(),
            windows: Vec::new(),
        };
        let attach = [
            "mycelium",
            "--realm",
            "work",
            "attach",
            "--host",
            "work-attended",
        ];
        let by_conversation = [&attach[..], &["--session", "c-01a1"]].concat();
        let by_id = [&attach[..], &["--session", "01a1"]].concat();
        let fell_back = ["pi", "--session", "/home/jitsusama/.pi/01a1.jsonl"];
        let other = [&attach[..], &["--session", "c-0e50"]].concat();
        assert_eq!(
            joined_by(
                &hosted(None),
                &[held("5b99", &other), held("0d1e", &by_conversation)]
            )
            .map(|one| one.id.as_str()),
            Some("0d1e"),
            "the holder whose command joins its conversation"
        );
        assert_eq!(
            joined_by(&hosted(None), &[held("0d1e", &by_id)]).map(|one| one.id.as_str()),
            Some("0d1e"),
            "or names the session by its own id"
        );
        assert_eq!(
            joined_by(
                &hosted(None),
                &[held("0d1e", &fell_back), held("5b99", &other)]
            ),
            None,
            "a join fallen back to plain pi, or one of another session, joins it to nothing"
        );
        assert_eq!(
            joined_by(
                &hosted(Some("equilibrius")),
                &[held("0d1e", &by_conversation)]
            ),
            None,
            "a session elsewhere is never in a window here"
        );
        let plain = Presence {
            body: json!({"session": "01a1", "owner": {"pid": 77}}),
            ..hosted(None)
        };
        assert_eq!(
            joined_by(&plain, &[held("0d1e", &by_id)]),
            None,
            "a session of its own is held by its process, never joined"
        );
    }

    #[test]
    fn a_hosted_session_is_viewed_by_the_words_its_presence_says() {
        let view = [
            "node",
            "host-join.ts",
            "--view",
            "--session",
            "01a1",
            "--",
            "pi",
        ];
        assert_eq!(
            viewed_by(&json!({"hosted": {"host": "work-attended", "view": view}})),
            Some(view.map(str::to_owned).to_vec()),
            "the join its presence names"
        );
        for said in [
            json!({"hosted": {"host": "work-attended"}}),
            json!({"hosted": {"host": "work-attended", "view": []}}),
            json!({"hosted": {"host": "work-attended", "view": ["node", 7]}}),
            json!({"hosted": {"host": "work-attended", "view": "node host-join.ts"}}),
            json!({"session": "01a1"}),
        ] {
            assert_eq!(viewed_by(&said), None, "no words of {said}");
        }
    }

    #[test]
    fn a_hosted_session_says_its_host_and_conversation() {
        let presence = |hosted: Value| Presence {
            session: "01a1".to_owned(),
            body: json!({
                "session": "01a1", "addresses": ["4UHC5Q"], "state": "idle",
                "owner": {"pid": 4242}, "hosted": hosted, "at": "2026-11-09T19:00:00Z",
            }),
            elsewhere: None,
            lives: true,
            unreachable_since: None,
        };
        let hosted = session(
            &presence(json!({"host": "lantern", "conversation": "c-01a1"})),
            "optimus",
        )
        .expect("a hosted presence reads");
        assert_eq!(
            (
                hosted.host.as_deref(),
                hosted.conversation.as_deref(),
                hosted.held.as_deref(),
                hosted.state,
            ),
            (Some("lantern"), Some("c-01a1"), None, State::Idle),
            "it lives while its host does, held by nothing of its own"
        );
        let unread = session(&presence(json!({"host": 7})), "optimus")
            .expect("a presence with a hosted part it can't read still reads");
        assert_eq!(
            (unread.host, unread.conversation),
            (None, None),
            "what it can't read of its host it leaves out"
        );
        let own = session(&presence(Value::Null), "optimus").expect("a presence reads");
        assert_eq!(
            (own.host, own.conversation),
            (None, None),
            "a session in a process of its own has no host"
        );
    }
}
