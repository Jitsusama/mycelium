//! The node's half of realm flows: sending a message into another realm
//! by a flow of this one's, taking one another realm's node hands across,
//! and handing across what waits for this machine to do it.

use std::time::Duration;

use serde_json::json;

use super::{Shared, millis_at, peer};
use crate::flow::{self, Code, Crossing, Kept, Refusal};
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
        Answer::Ok(Response::Sent(Box::new(message)))
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
        // Said as the file written here says it: by this node's process,
        // which hosts no conversation and invites no one, and with no
        // field this node doesn't read, as nothing unread crosses a realm.
        let taken = Message {
            from_realm: Some(from_realm.clone()),
            for_realm: None,
            supersedes: None,
            pid: Some(std::process::id()),
            hosted: false,
            invite: None,
            rest: serde_json::Map::new(),
            ..message
        };
        let verdict = self.may_take(&taken, &from_realm);
        if let Err(refusal) = verdict {
            self.keep(&taken, &from_realm, &self.realm, Err(&refusal));
            return refusal.into();
        }
        if self.store.message(&taken.id).is_some() {
            return Answer::Ok(Response::Sent(Box::new(taken)));
        }
        let body = mesh::crossed(&taken, &from_realm, std::process::id());
        if let Err(error) = self.store.put_message(&taken.id, &body) {
            return Answer::Err(format!("the store refused the message: {error}"));
        }
        self.keep(&taken, &from_realm, &self.realm, Ok(()));
        Answer::Ok(Response::Sent(Box::new(taken)))
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
        let Ok(sender) = peer(&message.from) else {
            eprintln!(
                "{} didn't cross into the {realm} realm ({why}), and its sender can't be told",
                message.id
            );
            return;
        };
        let draft = Draft {
            from: self.speaker(),
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
mod tests;
