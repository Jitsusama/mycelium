//! The node's half of realm flows: sending a message into another realm
//! by a flow of this one's, taking one another realm's node hands across,
//! and handing across what waits for this machine to do it.

use std::time::Duration;

use serde_json::json;

use super::{Shared, millis_at};
use crate::flow::{self, Code, Crossing, Kept, Refusal};
use crate::group::Group;
use crate::mesh;
use crate::record::{Intent, Message, State as SessionState};
use crate::store::{self, Store};
use crate::wire::{Answer, Draft, Response};

/// How long a message waits to cross before it is let go: the mesh keeps
/// a week of messages, and one older than that is gone from it anyway.
const WAITS_AT_MOST: Duration = Duration::from_secs(7 * 24 * 60 * 60);

impl<S: Store> Shared<S> {
    /// The realms a draft may go to, as this realm's flows say.
    fn may_send(&self, draft: &Draft, realm: &str) -> Result<(), Refusal> {
        if !self.flows.to.contains(realm) {
            return Err(Refusal::new(
                Code::NoFlow,
                format!(
                    "the {} realm has no flow into the {realm} realm, so nothing said here \
                     reaches it",
                    self.realm
                ),
            ));
        }
        flow::text_only(draft.intent, &draft.groups)
    }

    /// Sends `draft` into the realm it names, by a flow: handed to that
    /// realm's node here when this machine runs one, or else kept in this
    /// realm as waiting for it, for a link to carry to a machine that
    /// does. Nothing of a crossing made here stays in this realm, but for
    /// its line in each node's log.
    pub(super) async fn send_across(&self, draft: Draft) -> Answer {
        let realm = draft.realm.clone().unwrap_or_default();
        if let Err(why) = store::folders::safe(&draft.from) {
            return Answer::Err(format!("from: {why}"));
        }
        let (id, mut body) = mesh::written(&draft, std::process::id());
        let Some(mut message) = mesh::message(&body) else {
            return Answer::Err("a message needs at least one group".to_owned());
        };
        if let Err(refusal) = self.may_send(&draft, &realm) {
            self.keep(&message, &self.realm, &realm, Err(&refusal));
            return refusal.into();
        }
        if self.flows.socket(&realm).exists() {
            return self.hand_across(message, &realm).await;
        }
        body["forRealm"] = json!(realm);
        if let Err(error) = self.store.put_message(&id, &body) {
            return Answer::Err(format!("the store refused the message: {error}"));
        }
        message.for_realm = Some(realm);
        Answer::Ok(Response::Sent(message))
    }

    /// Hands `message` to `realm`'s node on this machine, keeping how that
    /// went. A node that fails to take it is taken as unreached, so what
    /// waits is tried again.
    async fn hand_across(&self, message: Message, realm: &str) -> Answer {
        let crossing = Crossing {
            message: message.clone(),
            from_realm: self.realm.clone(),
        };
        let answer = match flow::hand(&self.flows.socket(realm), crossing).await {
            Answer::Err(why) => Refusal::new(
                Code::Unreached,
                format!("the {realm} realm's node failed to take it: {why}"),
            )
            .into(),
            answer => answer,
        };
        let verdict = match &answer {
            Answer::Refused(refusal) => Err(refusal),
            _ => Ok(()),
        };
        self.keep(&message, &self.realm, realm, verdict);
        answer
    }

    /// Takes a message another realm's node on this machine hands across,
    /// under a flow from that realm into this one, keeping how that went.
    /// One already held, as when another machine crossed it first, is
    /// answered as sent and not kept again.
    pub(super) fn take(&self, crossing: Crossing) -> Answer {
        let Crossing {
            message,
            from_realm,
        } = crossing;
        let taken = Message {
            from_realm: Some(from_realm.clone()),
            for_realm: None,
            supersedes: None,
            ..message
        };
        let verdict = self.may_take(&taken, &from_realm);
        if let Err(refusal) = verdict {
            self.keep(&taken, &from_realm, &self.realm, Err(&refusal));
            return refusal.into();
        }
        if self.store.message(&taken.id).is_some() {
            return Answer::Ok(Response::Sent(taken));
        }
        let body = mesh::crossed(&taken, &from_realm, std::process::id());
        if let Err(error) = self.store.put_message(&taken.id, &body) {
            return Answer::Err(format!("the store refused the message: {error}"));
        }
        self.keep(&taken, &from_realm, &self.realm, Ok(()));
        Answer::Ok(Response::Sent(taken))
    }

    /// Whether this realm takes `message` from `from_realm`: only under a
    /// flow from it, only text, and only for a session here that hears it.
    fn may_take(&self, message: &Message, from_realm: &str) -> Result<(), Refusal> {
        if !self.flows.from.contains(from_realm) {
            return Err(Refusal::new(
                Code::NoFlow,
                format!(
                    "the {} realm takes nothing from the {from_realm} realm",
                    self.realm
                ),
            ));
        }
        flow::text_only(message.intent, &message.groups)?;
        for said in [&message.from, &message.id] {
            store::folders::safe(said)
                .map_err(|why| Refusal::new(Code::NotText, format!("{said:?}: {why}")))?;
        }
        let sessions = self.lock().state.sessions();
        let heard = message
            .groups
            .iter()
            .filter_map(flow::address)
            .any(|address| {
                sessions.iter().any(|session| {
                    session.state != SessionState::Ended
                        && session.address.eq_ignore_ascii_case(address)
                })
            });
        if heard {
            return Ok(());
        }
        let named: Vec<String> = message
            .groups
            .iter()
            .filter_map(flow::address)
            .map(str::to_uppercase)
            .collect();
        Err(Refusal::new(
            Code::Unheard,
            format!(
                "no session of the {} realm goes by {}",
                self.realm,
                named.join(" or ")
            ),
        ))
    }

    /// Holds each of `messages` that waits to cross by a flow of this
    /// realm's, and hands across those this machine can.
    pub(super) fn wait_to_cross(&self, messages: Vec<Message>) {
        {
            let mut inner = self.lock();
            for message in messages {
                let ours = message
                    .for_realm
                    .as_ref()
                    .is_some_and(|realm| self.flows.to.contains(realm));
                if ours && !inner.crossed.contains(&message.id) {
                    inner.waiting.insert(message.id.clone(), message);
                }
            }
        }
        self.cross_waiting();
    }

    /// Hands across each waiting message whose realm has a node on this
    /// machine, and lets go of any that has waited past the mesh's week.
    pub(super) fn cross_waiting(&self) {
        let oldest = jiff::Timestamp::now().as_millisecond()
            - i64::try_from(WAITS_AT_MOST.as_millis()).unwrap_or(i64::MAX);
        let ready: Vec<Message> = {
            let mut inner = self.lock();
            inner
                .waiting
                .retain(|_, message| millis_at(&message.at) >= oldest);
            let ready: Vec<Message> = inner
                .waiting
                .values()
                .filter(|message| {
                    message
                        .for_realm
                        .as_ref()
                        .is_some_and(|realm| self.flows.socket(realm).exists())
                })
                .cloned()
                .collect();
            for message in &ready {
                inner.waiting.remove(&message.id);
                inner.crossed.insert(message.id.clone());
            }
            ready
        };
        for message in ready {
            let shared = self.clone();
            tokio::spawn(async move { shared.cross_one(message).await });
        }
    }

    /// Hands one waiting message across: one unreached waits again, and
    /// one refused is told to its sender, in this realm, as a finding.
    async fn cross_one(&self, message: Message) {
        let realm = message.for_realm.clone().unwrap_or_default();
        match self.hand_across(message.clone(), &realm).await {
            Answer::Ok(_) => {}
            Answer::Refused(Refusal {
                code: Code::Unreached,
                ..
            }) => {
                let mut inner = self.lock();
                inner.crossed.remove(&message.id);
                inner.waiting.insert(message.id.clone(), message);
            }
            Answer::Refused(Refusal { why, .. }) | Answer::Err(why) => {
                self.tell_refused(&message, &realm, &why);
            }
        }
    }

    /// Tells the sender of `message` it didn't cross into `realm`, and why.
    fn tell_refused(&self, message: &Message, realm: &str, why: &str) {
        let Ok(sender) = Group::new(format!("peer.{}", message.from.to_lowercase())) else {
            eprintln!(
                "{} didn't cross into the {realm} realm ({why}), and its sender can't be told",
                message.id
            );
            return;
        };
        let draft = Draft {
            from: format!("mycelium@{}", self.machine.replace('-', "_")),
            groups: vec![sender],
            intent: Some(Intent::Finding),
            text: format!(
                "[{}] didn't cross into the {realm} realm: {why}",
                message.id
            ),
            re: Some(message.id.clone()),
            realm: None,
        };
        if let Err(error) = self.send(&draft) {
            eprintln!(
                "{} didn't cross into the {realm} realm, and its sender wasn't told: {error}",
                message.id
            );
        }
    }

    /// Keeps a line of the crossings log for `message`, from `from_realm`
    /// into `to_realm`, taken or refused as `verdict` says.
    fn keep(
        &self,
        message: &Message,
        from_realm: &str,
        to_realm: &str,
        verdict: Result<(), &Refusal>,
    ) {
        let line = Kept::of(message, from_realm, to_realm, verdict);
        if let Err(error) = flow::keep(&self.flows.log, &line) {
            eprintln!(
                "the crossing of {} wasn't kept in {}: {error}",
                message.id,
                self.flows.log.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::super::{Given, bind, run};
    use crate::awake::Awake;
    use crate::client::Client;
    use crate::flow::{Code, Flows};
    use crate::group::{Group, Pattern};
    use crate::link::dial::Ssh;
    use crate::link::discovery::Listed;
    use crate::link::membership::Realm;
    use crate::link::refused::Refusals;
    use crate::made::Book;
    use crate::record::{Intent, Message};
    use crate::store::Store;
    use crate::store::memory::Memory;
    use crate::wire::{Answer, Draft, Event, Request, Response};

    /// How long a test waits on a node before failing. Each wait ends the
    /// moment what it waits for happens, so this only bounds a hang, as
    /// long as the node's other tests give it for a machine under load.
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

    /// A realm's node on optimus, its socket among every realm's.
    struct Node {
        store: Memory,
        socket: PathBuf,
        log: PathBuf,
        _stop: tokio::sync::oneshot::Sender<()>,
    }

    /// Runs `realm`'s node in `dir`, with flows `to` and `from`, its
    /// realm's sessions going by `heard`.
    fn node(dir: &Path, realm: &str, to: &[&str], from: &[&str], heard: &[&str]) -> Node {
        let store = Memory::default();
        for address in heard {
            let presence = json!({
                "session": address.to_lowercase(),
                "addresses": [address],
                "at": "2026-11-03T21:00:00Z",
            });
            store.start_here(&address.to_lowercase(), presence, true);
        }
        let sockets = dir.join("mycelium");
        let socket = sockets.join(format!("{realm}.sock"));
        let log = dir.join(realm).join("crossings.jsonl");
        let realms = |names: &[&str]| names.iter().map(|&name| name.to_owned()).collect();
        let given = Given {
            sharing: None,
            starter: None,
            refusals: Refusals::default(),
            steers: dir.join(realm).join("steer"),
            awake: Awake::by(None),
            updater: "/nowhere".into(),
            serves: BTreeMap::new(),
            providers: Vec::new(),
            book: Book::at("/nowhere/made.jsonl".into()),
            flows: Flows {
                to: realms(to),
                from: realms(from),
                sockets,
                log: log.clone(),
            },
        };
        let membership = Realm {
            name: realm.to_owned(),
            machine: "optimus".to_owned(),
        };
        let bound = bind(&socket).expect("the socket binds");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn({
            let store = store.clone();
            async move {
                let stopped = async {
                    // A dropped sender stops the node as surely as a sent stop.
                    let _ = stopped.await;
                };
                run(
                    store,
                    membership,
                    Listed::<Ssh>(Vec::new()),
                    given,
                    bound,
                    stopped,
                )
                .await
            }
        });
        Node {
            store,
            socket,
            log,
            _stop: stop,
        }
    }

    fn peer(address: &str) -> Group {
        Group::new(format!("peer.{}", address.to_lowercase())).expect("a valid group")
    }

    fn draft(intent: Intent, to: Vec<Group>, realm: Option<&str>) -> Draft {
        Draft {
            from: "Q9METH".to_owned(),
            groups: to,
            intent: Some(intent),
            text: "the finding vanished".to_owned(),
            re: None,
            realm: realm.map(str::to_owned),
        }
    }

    /// What the node at `socket` answers `request`.
    async fn answer(socket: &Path, request: Request) -> Answer {
        let socket = socket.to_owned();
        let asked = tokio::task::spawn_blocking(move || {
            Client::connect(&socket)
                .expect("the client connects")
                .answer(&request)
                .expect("the node answers")
        });
        tokio::time::timeout(PATIENCE, asked)
            .await
            .expect("the node answers in time")
            .expect("the client asks")
    }

    /// The code of a refused answer.
    fn refused(answer: Answer) -> Code {
        match answer {
            Answer::Refused(refusal) => refusal.code,
            other => panic!("it was refused: {other:?}"),
        }
    }

    /// Each line of the crossings log at `log`.
    fn kept(log: &Path) -> Vec<Value> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }

    /// Watches the node at `socket`, once it is watching, until a message
    /// to a peer reaches it, answering it.
    async fn heard(socket: &Path) -> tokio::task::JoinHandle<Message> {
        let socket = socket.to_owned();
        let (watching, ready) = tokio::sync::oneshot::channel();
        let hearing = tokio::task::spawn_blocking(move || {
            let mut watch = Client::connect(&socket).expect("the watch connects");
            let answered = watch.ask(&Request::Watch {
                groups: Some(Pattern::new("peer.*").expect("a valid pattern")),
                recent: 0,
            });
            assert_eq!(answered.expect("the node answers"), Ok(Response::Watching));
            let _ = watching.send(());
            loop {
                match watch.next_event().expect("the watch reads") {
                    Some(Event::Message { message }) => return message,
                    Some(_) => {}
                    None => panic!("the node hung up before the message came"),
                }
            }
        });
        tokio::time::timeout(PATIENCE, ready)
            .await
            .expect("the node watches in time")
            .expect("the watch starts");
        hearing
    }

    #[tokio::test]
    async fn a_flow_carries_text_one_way_and_keeps_its_id_and_origin() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let personal = node(dir.path(), "personal", &["work"], &[], &["Q9METH"]);
        let work = node(dir.path(), "work", &[], &["personal"], &["16V6F4"]);

        let flows = answer(&personal.socket, Request::Flows).await;
        assert_eq!(
            flows,
            Answer::Ok(Response::Flows {
                to: vec!["work".to_owned()]
            })
        );
        let none = answer(&work.socket, Request::Flows).await;
        assert_eq!(none, Answer::Ok(Response::Flows { to: Vec::new() }));

        let finding = draft(Intent::Finding, vec![peer("16V6F4")], Some("work"));
        let Answer::Ok(Response::Sent(sent)) =
            answer(&personal.socket, Request::Send(finding)).await
        else {
            panic!("the finding crosses");
        };
        assert_eq!(sent.from_realm.as_deref(), Some("personal"));
        let there = work
            .store
            .message(&sent.id)
            .expect("it is in the work realm's mesh");
        assert_eq!(there["fromRealm"], "personal");
        assert_eq!(there["from"], "Q9METH");
        assert_eq!(there["to"], json!(["@16V6F4"]));
        assert_eq!(there["intent"], "finding");
        assert!(
            personal.store.message_ids(0).is_empty(),
            "nothing of the crossing stays in the realm it left"
        );
        for log in [&personal.log, &work.log] {
            let lines = kept(log);
            assert_eq!(lines.len(), 1, "each end keeps the crossing once");
            assert_eq!(lines[0]["id"], sent.id.as_str());
            assert_eq!(lines[0]["taken"], true);
        }

        let back = draft(Intent::Fyi, vec![peer("Q9METH")], Some("personal"));
        let code = refused(answer(&work.socket, Request::Send(back)).await);
        assert_eq!(code, Code::NoFlow, "the reverse is refused where it starts");
        assert!(
            personal.store.message_ids(0).is_empty(),
            "nothing reaches back"
        );
        assert_eq!(kept(&work.log)[1]["refused"]["code"], "no_flow");
    }

    #[tokio::test]
    async fn a_realm_takes_only_what_its_own_flow_lets_in_and_someone_there_hears() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // A work node whose own flow wrongly opens the reverse is still
        // refused by the personal realm, which takes nothing from work.
        let personal = node(dir.path(), "personal", &["work"], &[], &["Q9METH"]);
        let work = node(
            dir.path(),
            "work",
            &["personal"],
            &["personal"],
            &["16V6F4"],
        );

        let back = draft(Intent::Fyi, vec![peer("Q9METH")], Some("personal"));
        let code = refused(answer(&work.socket, Request::Send(back)).await);
        assert_eq!(code, Code::NoFlow, "the reverse is refused where it lands");
        assert!(
            personal.store.message_ids(0).is_empty(),
            "nothing reaches back"
        );
        assert_eq!(kept(&personal.log)[0]["refused"]["code"], "no_flow");

        let nobody = draft(Intent::Question, vec![peer("ZZZZZZ")], Some("work"));
        let code = refused(answer(&personal.socket, Request::Send(nobody)).await);
        assert_eq!(code, Code::Unheard, "no session there goes by it");
        assert!(
            work.store.message_ids(0).is_empty(),
            "an unheard message isn't kept"
        );
    }

    #[tokio::test]
    async fn only_a_fyi_finding_or_question_to_peers_crosses() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let personal = node(dir.path(), "personal", &["work"], &[], &["Q9METH"]);
        let work = node(dir.path(), "work", &[], &["personal"], &["16V6F4"]);
        let machine = Group::new("machine").expect("a valid group");
        let all = Group::new("all").expect("a valid group");
        let quest = Group::new("quest.wcjqsg").expect("a valid group");
        for refused_draft in [
            draft(Intent::Hold, vec![peer("16V6F4")], Some("work")),
            draft(Intent::Pivot, vec![peer("16V6F4")], Some("work")),
            draft(Intent::Fyi, vec![machine], Some("work")),
            draft(Intent::Fyi, vec![all], Some("work")),
            draft(Intent::Fyi, vec![peer("16V6F4"), quest], Some("work")),
        ] {
            let code = refused(answer(&personal.socket, Request::Send(refused_draft)).await);
            assert_eq!(code, Code::NotText);
        }

        // Handed straight to the work realm's node, as a faulty node might,
        // a hold is still refused there.
        let mut hold = Message {
            id: "1762200000000-Q9METH-1-00000001".to_owned(),
            at: "2026-11-03T21:00:00Z".to_owned(),
            from: "Q9METH".to_owned(),
            groups: vec![peer("16V6F4")],
            intent: Some(Intent::Hold),
            text: "hold".to_owned(),
            re: None,
            supersedes: None,
            from_realm: None,
            for_realm: None,
        };
        let crossing = crate::flow::Crossing {
            message: hold.clone(),
            from_realm: "personal".to_owned(),
        };
        assert_eq!(
            refused(answer(&work.socket, Request::Cross(crossing)).await),
            Code::NotText
        );
        assert!(
            work.store.message_ids(0).is_empty(),
            "nothing but text crosses"
        );

        hold.intent = Some(Intent::Question);
        let crossing = crate::flow::Crossing {
            message: hold.clone(),
            from_realm: "personal".to_owned(),
        };
        let Answer::Ok(Response::Sent(first)) =
            answer(&work.socket, Request::Cross(crossing.clone())).await
        else {
            panic!("a question crosses");
        };
        let Answer::Ok(Response::Sent(again)) =
            answer(&work.socket, Request::Cross(crossing)).await
        else {
            panic!("a question crossed twice is answered as sent");
        };
        assert_eq!(first.id, again.id, "it keeps its id");
        assert_eq!(work.store.message_ids(0).len(), 1, "it isn't written twice");
    }

    #[tokio::test]
    async fn a_message_naming_no_other_realm_never_crosses() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let personal = node(
            dir.path(),
            "personal",
            &["work"],
            &[],
            &["Q9METH", "16V6F4"],
        );
        let work = node(dir.path(), "work", &[], &["personal"], &["16V6F4"]);
        for realm in [None, Some("personal")] {
            let here = draft(Intent::Fyi, vec![peer("16V6F4")], realm);
            let Answer::Ok(Response::Sent(sent)) =
                answer(&personal.socket, Request::Send(here)).await
            else {
                panic!("a message within its realm is sent");
            };
            assert!(
                personal.store.message(&sent.id).is_some(),
                "it is in its own realm"
            );
            assert_eq!(sent.from_realm, None);
        }
        assert!(work.store.message_ids(0).is_empty(), "nothing crossed");
        assert!(
            kept(&personal.log).is_empty(),
            "nothing was judged a crossing"
        );
    }

    #[tokio::test]
    async fn a_message_a_link_carried_crosses_where_both_realms_run() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // methuselah runs no work node, so what it sends waits for one.
        let methuselah = tempfile::tempdir().expect("a temporary folder");
        let alone = node(methuselah.path(), "personal", &["work"], &[], &["Q9METH"]);
        let finding = draft(Intent::Finding, vec![peer("16V6F4")], Some("work"));
        let Answer::Ok(Response::Sent(waiting)) =
            answer(&alone.socket, Request::Send(finding)).await
        else {
            panic!("a message for a realm no node here runs waits");
        };
        assert_eq!(waiting.for_realm.as_deref(), Some("work"));
        let body = alone
            .store
            .message(&waiting.id)
            .expect("it waits in its own realm");
        assert_eq!(body["forRealm"], "work");
        let listed = answer(
            &alone.socket,
            Request::Messages {
                groups: Pattern::everything(),
                limit: 10,
            },
        )
        .await;
        assert_eq!(
            listed,
            Answer::Ok(Response::Messages(Vec::new())),
            "no one here hears it"
        );

        // A link carries it to optimus, which runs both realms' nodes.
        let personal = node(dir.path(), "personal", &["work"], &[], &[]);
        let work = node(dir.path(), "work", &[], &["personal"], &["16V6F4"]);
        let hearing = heard(&work.socket).await;
        personal.store.send_here(&waiting.id, body);
        let crossed = tokio::time::timeout(PATIENCE, hearing)
            .await
            .expect("it crosses in time")
            .expect("the watch reads");
        assert_eq!(crossed.id, waiting.id, "it keeps its id");
        assert_eq!(crossed.from_realm.as_deref(), Some("personal"));
        assert_eq!(crossed.for_realm, None);

        // One the work realm refuses is told to its sender, in its own realm.
        let nobody = draft(Intent::Question, vec![peer("ZZZZZZ")], Some("work"));
        let Answer::Ok(Response::Sent(unheard)) =
            answer(&alone.socket, Request::Send(nobody)).await
        else {
            panic!("it waits");
        };
        let body = alone
            .store
            .message(&unheard.id)
            .expect("it waits in its own realm");
        let telling = heard(&personal.socket).await;
        personal.store.send_here(&unheard.id, body);
        let told = tokio::time::timeout(PATIENCE, telling)
            .await
            .expect("its sender is told in time")
            .expect("the watch reads");
        assert_eq!(told.from, "mycelium@optimus");
        assert_eq!(told.groups, vec![peer("Q9METH")]);
        assert_eq!(told.intent, Some(Intent::Finding));
        assert_eq!(told.re.as_deref(), Some(unheard.id.as_str()));
        assert!(work.store.message(&unheard.id).is_none(), "it didn't cross");
    }
}
