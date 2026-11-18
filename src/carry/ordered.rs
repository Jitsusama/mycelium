//! A carry an orchestrator orders under its own name, never as the person:
//! the steer act `ordered-carry`, which nothing that trusts the person's
//! own `carry` trusts.
//!
//! Who may order one is read from the node's own record of who told it
//! they orchestrate which quests (`carry::chain`), never from what a
//! session says of itself, and checked here against the quests the node
//! serves, each naming the sessions attached to it and the orchestrator
//! above it, which must agree. A session's
//! carry may be ordered by a session attached to its quest's orchestrator,
//! or to that one's orchestrator, and so on up the chain; nobody else's
//! word, a presence's or `PI_QUEST`'s, counts. Each carry ordered is kept
//! as a line, so the person sees who ordered it and why.
//!
//! The quests' record is a file any process of the person's account can
//! write, so the chain it gives is believed only where the mesh agrees:
//! each running session of a quest in the chain is in its orchestrator's
//! `#orchestration/` group, as its presence gives the groups the mesh put
//! it in, and so is the session asking. An index rewritten, or one gone
//! stale, is then refused. Both are worked out by pi from the quests'
//! own front matter, so an edit there reaches both in time: they are a
//! check on the node's own record, never proof.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use serde_json::Value;

/// The steer act an orchestrator orders a carry with.
pub const ACT: &str = "ordered-carry";

/// A session, as the node's record and the kernel tell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Known<'a> {
    /// Its own id, as its presence and the quests' records name it.
    pub session: &'a str,
    /// The address it goes by.
    pub address: &'a str,
}

/// Who a carry is ordered by, as the record proves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// The address of the orchestrating quest, which the steer and its
    /// record name as who ordered it.
    pub by: String,
    /// The id of the quest the carried session works on.
    pub quest: String,
    /// Each quest from that one up to the one ordering, with the address
    /// of the orchestrator its record names, nearest first.
    pub chain: Vec<Link>,
}

/// One step of a chain of orchestrators: a quest, and its orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The quest's id.
    pub quest: String,
    /// The address of the orchestrator its record names.
    pub orchestrator: String,
}

/// Whether the mesh agrees with `order`'s chain: every session of each
/// quest in it that `running` holds, by its id, with the groups its
/// presence says the mesh put it in, is in that quest's orchestrator's
/// `#orchestration/` group, at least one does for each, and `asker` is in
/// the ordering orchestrator's own. Refused, saying where they disagree
/// or where it can't be told.
///
/// # Errors
///
/// When the mesh disagrees with the record, or can't say.
pub fn agreed(
    order: &Order,
    asker: Known,
    quests: &[Value],
    running: &BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    for link in &order.chain {
        let group = orchestration(&link.orchestrator);
        let record = quests
            .iter()
            .find(|quest| id_of(quest) == Some(link.quest.as_str()));
        let sessions: Vec<&Vec<String>> = record
            .and_then(|quest| quest.get("sessions"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|session| running.get(session))
            .collect();
        if sessions.is_empty() {
            return Err(format!(
                "no session of {} runs to say the mesh puts it in {group}, so the chain can't \
                 be checked",
                link.quest
            ));
        }
        if sessions.iter().any(|groups| !groups.contains(&group)) {
            return Err(format!(
                "the mesh doesn't put {}'s session in {group}, as the quests' record says {}'s \
                 orchestrator is {}, so the chain isn't believed",
                record.and_then(address_of).unwrap_or(&link.quest),
                link.quest,
                link.orchestrator
            ));
        }
    }
    let own = orchestration(&order.by);
    if !running
        .get(asker.session)
        .is_some_and(|groups| groups.contains(&own))
    {
        return Err(format!(
            "the mesh doesn't put {}'s asking session in {own}, so the chain isn't believed",
            asker.address
        ));
    }
    Ok(())
}

/// The mesh's group of the orchestration `address` heads.
fn orchestration(address: &str) -> String {
    format!("#orchestration/{}", address.to_ascii_lowercase())
}

/// Whether `asker` may order the carry of `session`, by `quests`, the
/// quests' records the node serves: `asker` must be among the sessions of
/// a quest up the chain of orchestrators above the one quest whose record
/// holds `session`. Refused, saying why and who could order it, when it
/// isn't, or when the record can't tell.
///
/// # Errors
///
/// When the record doesn't prove the chain.
pub fn ordered_by(asker: Known, session: Known, quests: &[Value]) -> Result<Order, String> {
    let target = holding(session, quests)?;
    let quest = id_of(target).unwrap_or_default().to_owned();
    // Up the chain of orchestrators, each found by its address, until one
    // attaches the asker, the chain ends, or it comes round again.
    let mut chain: Vec<&str> = Vec::new();
    let mut links = Vec::new();
    let mut passed = BTreeSet::new();
    let mut missing = None;
    let mut at = target;
    while let Some(above) = at.get("orchestrator").and_then(Value::as_str) {
        if !passed.insert(above) {
            break;
        }
        links.push(Link {
            quest: id_of(at).unwrap_or_default().to_owned(),
            orchestrator: above.to_ascii_uppercase(),
        });
        let Some(orchestrating) = quests
            .iter()
            .find(|quest| address_of(quest).is_some_and(|it| it.eq_ignore_ascii_case(above)))
        else {
            missing = Some(above);
            break;
        };
        if attaches(orchestrating, asker.session) {
            return Ok(Order {
                by: above.to_ascii_uppercase(),
                quest,
                chain: links,
            });
        }
        chain.push(above);
        at = orchestrating;
    }
    if chain.is_empty() && missing.is_none() {
        return Err(format!(
            "{quest} has no orchestrator by the quests' record, so only the person carries {}",
            session.address
        ));
    }
    let refused = format!(
        "{} doesn't orchestrate {quest} by the quests' record, so it can't order {}'s carry",
        asker.address, session.address
    );
    let could =
        (!chain.is_empty()).then(|| format!("only a session of {} could", chain.join(" or ")));
    let unrecorded = missing.map(|address| format!("{address} has no quest's record"));
    let said: Vec<String> = could.into_iter().chain(unrecorded).collect();
    Err(format!("{refused}: {}", said.join(", and ")))
}

/// The id of the one quest whose record, among `quests`, holds `session`.
///
/// # Errors
///
/// When none does, or more than one.
pub fn quest_of(session: Known, quests: &[Value]) -> Result<String, String> {
    holding(session, quests).map(|quest| id_of(quest).unwrap_or_default().to_owned())
}

/// Whether any quest's record among `quests` holds the session `session`.
#[must_use]
pub fn holds(session: &str, quests: &[Value]) -> bool {
    quests.iter().any(|quest| attaches(quest, session))
}

/// The id of the one quest among `ended`, the quests that ended, whose
/// record holds `session`, and the address of who orchestrated it, as
/// the harness wrote it as the quest ended.
///
/// # Errors
///
/// When none holds it, more than one does, or it names no orchestrator.
pub fn ended_holding(session: Known, ended: &[Value]) -> Result<(String, String), String> {
    let quest = holding(session, ended)?;
    let id = id_of(quest).unwrap_or_default().to_owned();
    let orchestrator = quest
        .get("orchestrator")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!("{id} ended naming no orchestrator, so only the person ends its sessions")
        })?;
    Ok((id, orchestrator.to_ascii_uppercase()))
}

/// The id of the quest among `quests` that goes by `address`.
#[must_use]
pub fn going_by<'a>(address: &str, quests: &'a [Value]) -> Option<&'a str> {
    quests
        .iter()
        .find(|quest| address_of(quest).is_some_and(|it| it.eq_ignore_ascii_case(address)))
        .and_then(id_of)
}

/// The one quest's record among `quests` that holds `session`.
fn holding<'a>(session: Known, quests: &'a [Value]) -> Result<&'a Value, String> {
    let holding: Vec<&Value> = quests
        .iter()
        .filter(|quest| attaches(quest, session.session))
        .collect();
    match holding.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!(
            "no quest's record holds {}'s session, so who orchestrates it can't be told",
            session.address
        )),
        many => {
            let ids: Vec<&str> = many.iter().filter_map(|quest| id_of(quest)).collect();
            Err(format!(
                "{}'s session is held by {}, so who orchestrates it can't be told",
                session.address,
                ids.join(" and ")
            ))
        }
    }
}

/// Whether `quest`'s record attaches the session `session`.
fn attaches(quest: &Value, session: &str) -> bool {
    quest
        .get("sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| sessions.iter().any(|one| one.as_str() == Some(session)))
}

/// A quest's id, as its record names it.
fn id_of(quest: &Value) -> Option<&str> {
    quest.get("id").and_then(Value::as_str)
}

/// A quest's address, as its record names it, else its id's last part.
fn address_of(quest: &Value) -> Option<&str> {
    quest
        .get("address")
        .and_then(Value::as_str)
        .or_else(|| id_of(quest)?.rsplit('-').next())
}

/// Keeps `line`, one carry ordered, as a line of its own in `file`,
/// making its folder where it isn't yet.
///
/// # Errors
///
/// When the file can't be written.
pub fn keep(file: &Path, line: &Value) -> io::Result<()> {
    if let Some(folder) = file.parent() {
        fs::create_dir_all(folder)?;
    }
    let mut kept = OpenOptions::new().create(true).append(true).open(file)?;
    // One write a line, so two carries kept at once never interleave.
    kept.write_all(format!("{line}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::{Known, Link, Order, agreed, keep, ordered_by};

    fn link(quest: &str, orchestrator: &str) -> Link {
        Link {
            quest: quest.to_owned(),
            orchestrator: orchestrator.to_owned(),
        }
    }

    /// The sessions running, each with the groups the mesh put it in, as
    /// the realm of `quests` has them.
    fn running() -> BTreeMap<String, Vec<String>> {
        [
            ("s-steward", vec!["#orchestration/wcjqsg"]),
            ("s-owner", vec!["#orchestration/wcjqsg"]),
            (
                "s-weigh",
                vec!["#orchestration/ospht4", "#orchestration/wcjqsg"],
            ),
            ("s-harbour", vec!["#orchestration/wcjqsg", "#repo/mycelium"]),
            ("s-aquifer", vec!["#orchestration/wm4ksz"]),
            ("s-threads", vec!["#orchestration/wm4ksz"]),
        ]
        .into_iter()
        .map(|(session, groups)| {
            (
                session.to_owned(),
                groups.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect()
    }

    #[test]
    fn the_chain_is_believed_where_the_mesh_agrees() {
        let quests = quests();
        let order = ordered_by(
            known("s-steward", "WCJQSG"),
            known("s-harbour", "HARBOR"),
            &quests,
        )
        .expect("the record proves it");
        assert_eq!(
            agreed(&order, known("s-steward", "WCJQSG"), &quests, &running()),
            Ok(()),
            "HARBOR's session is in WCJQSG's orchestration, and so is WCJQSG's"
        );
    }

    #[test]
    fn a_session_that_rewrites_its_quests_orchestrator_neither_orders_nor_is_carried() {
        // OSPHT4 names itself HARBOR's orchestrator in the quests' record,
        // which the record alone would take.
        let mut forged = quests();
        forged[3]["orchestrator"] = json!("OSPHT4");
        let order = ordered_by(
            known("s-weigh", "OSPHT4"),
            known("s-harbour", "HARBOR"),
            &forged,
        )
        .expect("the forged record says so");
        assert_eq!(
            agreed(&order, known("s-weigh", "OSPHT4"), &forged, &running()),
            Err(
                "the mesh doesn't put HARBOR's session in #orchestration/ospht4, as the quests' \
                 record says QEST-20261115-HARBOR's orchestrator is OSPHT4, so the chain isn't \
                 believed"
                    .to_owned()
            ),
            "a forged orchestrator orders nothing"
        );
        // THREAD names WCJQSG its orchestrator, to be carried on its word.
        let mut forged = quests();
        forged[5]["orchestrator"] = json!("WCJQSG");
        let order = ordered_by(
            known("s-steward", "WCJQSG"),
            known("s-threads", "THREAD"),
            &forged,
        )
        .expect("the forged record says so");
        assert!(
            agreed(&order, known("s-steward", "WCJQSG"), &forged, &running())
                .is_err_and(|why| why.contains("#orchestration/wcjqsg")),
            "nor is a session carried on a chain it forged"
        );
    }

    #[test]
    fn a_chain_the_mesh_cant_speak_for_isnt_believed() {
        let mut quests = quests();
        quests[0]["orchestrator"] = json!("WM4KSZ");
        quests[4]["orchestrator"] = json!(null);
        let order = ordered_by(
            known("s-aquifer", "WM4KSZ"),
            known("s-harbour", "HARBOR"),
            &quests,
        )
        .expect("the record proves it");
        let mut running = running();
        running.insert(
            "s-steward".to_owned(),
            vec!["#orchestration/wm4ksz".to_owned()],
        );
        assert_eq!(
            agreed(&order, known("s-aquifer", "WM4KSZ"), &quests, &running),
            Ok(()),
            "each step up the chain agrees"
        );
        running.remove("s-steward");
        assert_eq!(
            agreed(&order, known("s-aquifer", "WM4KSZ"), &quests, &running),
            Err(
                "no session of QEST-20261007-WCJQSG runs to say the mesh puts it in \
                 #orchestration/wm4ksz, so the chain can't be checked"
                    .to_owned()
            ),
            "a step no running session speaks for can't be told"
        );
        let mut running = self::running();
        running.insert(
            "s-steward".to_owned(),
            vec!["#orchestration/wm4ksz".to_owned()],
        );
        running.insert("s-aquifer".to_owned(), Vec::new());
        assert_eq!(
            agreed(&order, known("s-aquifer", "WM4KSZ"), &quests, &running),
            Err(
                "the mesh doesn't put WM4KSZ's asking session in #orchestration/wm4ksz, so the \
                 chain isn't believed"
                    .to_owned()
            ),
            "nor an asker the mesh doesn't put in its own orchestration"
        );
    }

    /// The quests' records of a realm where WCJQSG orchestrates MYCELI,
    /// MYCELI holds HARBOR, and WM4KSZ stands alone above THREAD, each
    /// with the sessions attached to it.
    fn quests() -> Vec<Value> {
        vec![
            json!({"id": "QEST-20261007-WCJQSG", "address": "WCJQSG", "orchestrator": null,
                   "sessions": ["s-steward"]}),
            json!({"id": "QEST-20261013-MYCELI", "address": "MYCELI", "orchestrator": "WCJQSG",
                   "parent": "QEST-20261007-WCJQSG", "sessions": ["s-owner"]}),
            json!({"id": "QEST-20261108-OSPHT4", "address": "OSPHT4", "orchestrator": "WCJQSG",
                   "parent": "QEST-20261007-WCJQSG", "sessions": ["s-weigh"]}),
            json!({"id": "QEST-20261115-HARBOR", "address": "HARBOR", "orchestrator": "WCJQSG",
                   "parent": "QEST-20261013-MYCELI", "sessions": ["s-harbour"]}),
            json!({"id": "QEST-20261109-WM4KSZ", "address": "WM4KSZ", "orchestrator": null,
                   "sessions": ["s-aquifer"]}),
            json!({"id": "QEST-20261109-THREAD", "address": "THREAD", "orchestrator": "WM4KSZ",
                   "parent": "QEST-20261109-WM4KSZ", "sessions": ["s-threads"]}),
        ]
    }

    fn known<'a>(session: &'a str, address: &'a str) -> Known<'a> {
        Known { session, address }
    }

    #[test]
    fn its_quests_orchestrator_may_order_a_sessions_carry() {
        assert_eq!(
            ordered_by(
                known("s-steward", "WCJQSG"),
                known("s-harbour", "HARBOR"),
                &quests()
            ),
            Ok(Order {
                by: "WCJQSG".to_owned(),
                quest: "QEST-20261115-HARBOR".to_owned(),
                chain: vec![link("QEST-20261115-HARBOR", "WCJQSG")],
            }),
            "WCJQSG orchestrates HARBOR, by HARBOR's record"
        );
        assert_eq!(
            ordered_by(
                known("s-aquifer", "WM4KSZ"),
                known("s-threads", "THREAD"),
                &quests()
            ),
            Ok(Order {
                by: "WM4KSZ".to_owned(),
                quest: "QEST-20261109-THREAD".to_owned(),
                chain: vec![link("QEST-20261109-THREAD", "WM4KSZ")],
            }),
            "and WM4KSZ orchestrates THREAD"
        );
    }

    #[test]
    fn an_orchestrator_up_the_chain_may_order_it_too() {
        let mut quests = quests();
        quests[0]["orchestrator"] = json!("WM4KSZ");
        quests[4]["orchestrator"] = json!(null);
        assert_eq!(
            ordered_by(
                known("s-aquifer", "WM4KSZ"),
                known("s-harbour", "HARBOR"),
                &quests
            )
            .map(|order| (order.by, order.chain)),
            Ok((
                "WM4KSZ".to_owned(),
                vec![
                    link("QEST-20261115-HARBOR", "WCJQSG"),
                    link("QEST-20261007-WCJQSG", "WM4KSZ")
                ]
            )),
            "an orchestrator above WCJQSG orders it, under its own name"
        );
    }

    #[test]
    fn a_session_outside_the_chain_is_refused_naming_who_could_order_it() {
        assert_eq!(
            ordered_by(
                known("s-weigh", "OSPHT4"),
                known("s-threads", "THREAD"),
                &quests()
            ),
            Err(
                "OSPHT4 doesn't orchestrate QEST-20261109-THREAD by the quests' record, so it \
                 can't order THREAD's carry: only a session of WM4KSZ could"
                    .to_owned()
            ),
            "another orchestrator's word isn't in the chain"
        );
        assert_eq!(
            ordered_by(
                known("s-owner", "MYCELI"),
                known("s-harbour", "HARBOR"),
                &quests()
            ),
            Err(
                "MYCELI doesn't orchestrate QEST-20261115-HARBOR by the quests' record, so it \
                 can't order HARBOR's carry: only a session of WCJQSG could"
                    .to_owned()
            ),
            "nor is a parent quest that doesn't orchestrate"
        );
    }

    #[test]
    fn a_claim_the_record_doesnt_hold_proves_nothing() {
        // A session whose presence or PI_QUEST says WCJQSG, but which the
        // record doesn't attach to it, is no orchestrator of anything.
        assert!(
            ordered_by(
                known("s-claims", "WCJQSG"),
                known("s-harbour", "HARBOR"),
                &quests()
            )
            .is_err(),
            "an address alone proves nothing"
        );
        assert_eq!(
            ordered_by(
                known("s-steward", "WCJQSG"),
                known("s-stray", "STRAY1"),
                &quests()
            ),
            Err(
                "no quest's record holds STRAY1's session, so who orchestrates it can't be told"
                    .to_owned()
            ),
            "nor is a session no record holds carried on its own say"
        );
        let mut twice = quests();
        twice[5]["sessions"] = json!(["s-threads", "s-harbour"]);
        assert_eq!(
            ordered_by(
                known("s-steward", "WCJQSG"),
                known("s-harbour", "HARBOR"),
                &twice
            ),
            Err(
                "HARBOR's session is held by QEST-20261115-HARBOR and QEST-20261109-THREAD, \
                 so who orchestrates it can't be told"
                    .to_owned()
            ),
            "nor one two records hold"
        );
    }

    #[test]
    fn a_quest_with_no_orchestrator_is_the_persons_alone_to_carry() {
        assert_eq!(
            ordered_by(
                known("s-steward", "WCJQSG"),
                known("s-aquifer", "WM4KSZ"),
                &quests()
            ),
            Err(
                "QEST-20261109-WM4KSZ has no orchestrator by the quests' record, so only the \
                 person carries WM4KSZ"
                    .to_owned()
            ),
            "an orchestrator's own session goes only on the person's word"
        );
    }

    #[test]
    fn a_loop_or_a_missing_record_ends_the_chain() {
        let mut looped = quests();
        looped[0]["orchestrator"] = json!("MYCELI");
        looped[1]["orchestrator"] = json!("WCJQSG");
        assert!(
            ordered_by(
                known("s-weigh", "OSPHT4"),
                known("s-harbour", "HARBOR"),
                &looped
            )
            .is_err_and(|why| why.ends_with("only a session of WCJQSG or MYCELI could")),
            "a loop of orchestrators ends where it began"
        );
        let mut missing = quests();
        missing.remove(4);
        assert!(
            ordered_by(
                known("s-aquifer", "WM4KSZ"),
                known("s-threads", "THREAD"),
                &missing
            )
            .is_err_and(|why| why.contains("WM4KSZ has no quest's record")),
            "an orchestrator with no record of its own attaches nobody"
        );
    }

    #[test]
    fn each_carry_ordered_is_kept_as_a_line() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("state").join("carries.jsonl");
        let first = json!({"by": "WCJQSG", "address": "HARBOR", "why": "its camera"});
        let second = json!({"by": "WM4KSZ", "address": "THREAD", "why": "the Mac"});
        keep(&file, &first).expect("kept");
        keep(&file, &second).expect("kept");
        let lines: Vec<Value> = std::fs::read_to_string(&file)
            .expect("read")
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON"))
            .collect();
        assert_eq!(lines, [first, second], "each whole, in order");
    }
}
