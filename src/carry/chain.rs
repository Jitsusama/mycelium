//! Who orchestrates which quests, as this node recorded it when each
//! orchestration was made, never as a session says it later.
//!
//! An orchestrator's session tells its node, over its own connection,
//! which quest it heads and which quests it orchestrates, once it loads
//! the quest and again whenever those change. The node proves which
//! session asks, from the kernel and pi's own records, and keeps each
//! claim in a file of its own. A quest another session already holds by
//! that record is refused: only its session hands it on, by releasing it
//! or by being continued, as a session rolled over into a new log that
//! names it as its `parentSession`; a claim whose session no longer
//! stands passes to the one that continues it, whether or not that one
//! tells the node again. An ordered carry's chain of
//! orchestrators is walked through this record, so neither a rewritten
//! quests' index nor an edited quest README moves it.
//!
//! Every file here is one any process of the person's account can write,
//! so the record holds against a fenced session only where the fence
//! keeps it read-only. Who asks is proved as far as the kernel and pi's
//! records go: a session's log held open by its process says "this
//! process says so", which stops a confused helper, not a hostile pi.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ordered::{Known, Link, Order};

/// Says where an ordered carry's chain came from, for its record.
pub const SOURCE: &str = "this node's record of orchestrations, checked against the quests' \
                          record and the mesh's orchestration groups";

/// Every orchestration this node recorded, by the orchestrating session's
/// id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orchestrations {
    /// Each orchestrating session's claim, by its id.
    pub sessions: BTreeMap<String, Claim>,
}

/// What one orchestrating session claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The address it goes by, as the mesh names it.
    pub address: String,
    /// The quest it heads, the orchestrating one it has loaded.
    pub heads: String,
    /// The quests it orchestrates, as it last said them.
    pub quests: BTreeSet<String>,
    /// When it last said them.
    pub at: jiff::Timestamp,
}

impl Orchestrations {
    /// The record kept at `file`, or none yet where nothing is kept.
    ///
    /// # Errors
    ///
    /// When the file can't be read, or isn't in this shape: a record
    /// that can't be read proves no chain.
    pub fn read(file: &Path) -> io::Result<Self> {
        match fs::read(file) {
            Ok(text) => serde_json::from_slice(&text).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Keeps the record at `file`, whole, so no reader sees half of it.
    ///
    /// # Errors
    ///
    /// When the file can't be written.
    pub fn keep(&self, file: &Path) -> io::Result<()> {
        if let Some(folder) = file.parent() {
            fs::create_dir_all(folder)?;
        }
        let fresh = file.with_extension("json.new");
        fs::write(
            &fresh,
            serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
        )?;
        fs::rename(fresh, file)
    }

    /// Records that `asker` heads `heads` and orchestrates `quests`, as of
    /// `at`, in place of what it claimed before. `continues` is the
    /// session the asker's own log says it continues, whose claims it
    /// takes on. Refused, changing nothing, where another session holds
    /// any of them.
    ///
    /// # Errors
    ///
    /// When another session holds one of the quests.
    pub fn claim(
        &mut self,
        asker: Known,
        heads: &str,
        quests: &[String],
        continues: Option<&str>,
        at: jiff::Timestamp,
    ) -> Result<(), String> {
        let own = |session: &str| session == asker.session || Some(session) == continues;
        let held = |quest: &str, heading: bool| {
            self.sessions.iter().find(|(session, claim)| {
                !own(session)
                    && if heading {
                        claim.heads == quest
                    } else {
                        claim.quests.contains(quest)
                    }
            })
        };
        let claimed = std::iter::once((heads, true))
            .chain(quests.iter().map(|quest| (quest.as_str(), false)));
        for (quest, heading) in claimed {
            if let Some((_, holder)) = held(quest, heading) {
                let what = if heading { "heads" } else { "orchestrates" };
                return Err(format!(
                    "{}'s session already {what} {quest} by this node's record, so {} can't: \
                     only that session hands it on, or the person",
                    holder.address, asker.address
                ));
            }
        }
        if let Some(continued) = continues {
            self.sessions.remove(continued);
        }
        self.sessions.insert(
            asker.session.to_owned(),
            Claim {
                address: asker.address.to_owned(),
                heads: heads.to_owned(),
                quests: quests.iter().cloned().collect(),
                at,
            },
        );
        Ok(())
    }

    /// Forgets what `session` claimed, as it unloads or concludes its
    /// quest; it claimed nothing more.
    pub fn release(&mut self, session: &str) {
        self.sessions.remove(session);
    }

    /// Frees `quest` from whichever session holds it, as the person asks,
    /// answering that session's address: one that heads it lets go of
    /// everything it claimed, since what it orchestrates hangs from it,
    /// and one that orchestrates it lets go of that quest alone.
    ///
    /// # Errors
    ///
    /// When no session holds `quest`, so nothing is freed.
    pub fn free(&mut self, quest: &str) -> Result<String, String> {
        if let Some(session) = self
            .sessions
            .iter()
            .find(|(_, claim)| claim.heads == quest)
            .map(|(session, _)| session.clone())
        {
            return Ok(self
                .sessions
                .remove(&session)
                .map(|claim| claim.address)
                .unwrap_or_default());
        }
        self.sessions
            .values_mut()
            .find_map(|claim| claim.quests.remove(quest).then(|| claim.address.clone()))
            .ok_or_else(|| {
                format!("no session holds {quest} by this node's record, so nothing is freed")
            })
    }

    /// Hands on or forgets what each session claimed that no longer runs
    /// or waits parked, as `standing` names those that do, each with the
    /// address the node knows it by, where it knows one.
    ///
    /// A lapsing claim passes, keeping what it heads and its quests, to
    /// the one standing session whose log continues the claimer's, as
    /// `continues` tells what each session's log continues: directly, or
    /// through logs that claimed nothing and no longer stand, up to
    /// [`CONTINUED_MOST`] logs back. It takes that session's address
    /// where the node knows it, else keeps its own. A session that has
    /// claimed for itself takes nothing, as its own word stands, and a
    /// claim two standing sessions continue passes to neither, since
    /// which goes on can't be told. Any other lapses, so an orchestrator
    /// started afresh, which continues no session, may claim its quests
    /// again. `continues` is asked only where some claim lapses, so the
    /// logs are read only then, and once.
    pub fn lapse<Continues: Fn(&str) -> Option<String>>(
        &mut self,
        standing: &BTreeMap<String, Option<String>>,
        continues: impl FnOnce() -> Continues,
    ) {
        let (kept, lapsing): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut self.sessions)
            .into_iter()
            .partition(|(session, _)| standing.contains_key(session));
        self.sessions = kept;
        if lapsing.is_empty() {
            return;
        }
        let continues = continues();
        let mut heirs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for session in standing.keys() {
            if self.sessions.contains_key(session) {
                continue;
            }
            let mut at = session.clone();
            for _ in 0..CONTINUED_MOST {
                let Some(before) = continues(&at) else {
                    break;
                };
                if let Some((claimer, _)) = lapsing.get_key_value(&before) {
                    heirs.entry(claimer).or_default().push(session);
                    break;
                }
                // A standing session's claim, or its having none, is its
                // own: what it continues isn't handed past it.
                if standing.contains_key(&before) {
                    break;
                }
                at = before;
            }
        }
        for (claimer, heirs) in heirs {
            let ([heir], Some(claim)) = (heirs.as_slice(), lapsing.get(claimer)) else {
                continue;
            };
            let address = standing
                .get(*heir)
                .cloned()
                .flatten()
                .unwrap_or_else(|| claim.address.clone());
            self.sessions.insert(
                (*heir).to_owned(),
                Claim {
                    address,
                    ..claim.clone()
                },
            );
        }
    }

    /// The quest the session `session` heads, by this record.
    #[must_use]
    pub fn heads(&self, session: &str) -> Option<&str> {
        self.sessions.get(session).map(|claim| claim.heads.as_str())
    }

    /// Who orchestrates `quest` up to `asker`, by this record: the
    /// session that orchestrates it, then the one orchestrating the quest
    /// that session heads, and so on, until one is `asker`. Refused,
    /// saying where the record stops, when none is.
    ///
    /// # Errors
    ///
    /// When the record doesn't lead from `quest` to `asker`.
    pub fn chain(&self, quest: &str, asker: Known) -> Result<Order, String> {
        let mut links = Vec::new();
        let mut passed = BTreeSet::new();
        let mut at = quest;
        while passed.insert(at) {
            let Some((session, claim)) = self
                .sessions
                .iter()
                .find(|(_, claim)| claim.quests.contains(at))
            else {
                break;
            };
            links.push(Link {
                quest: at.to_owned(),
                orchestrator: claim.address.clone(),
            });
            if session == asker.session {
                return Ok(Order {
                    by: claim.address.clone(),
                    quest: quest.to_owned(),
                    chain: links,
                });
            }
            at = &claim.heads;
        }
        if links.is_empty() {
            return Err(format!(
                "no orchestrator has told this node it orchestrates {quest}, so only the \
                 person carries it"
            ));
        }
        let could: Vec<String> = links
            .iter()
            .map(|link| format!("{}'s session", link.orchestrator))
            .collect();
        Err(format!(
            "{} doesn't orchestrate {quest} by this node's record, so it can't order its \
             carry: only {} could",
            asker.address,
            could.join(" or ")
        ))
    }
}

/// The session the log of `session` continues, as its own first line
/// names it, looking among the logs under `logs`, as [`Logs`] finds
/// them. None where no such log is found, or where it continues none.
#[must_use]
pub fn continues(logs: &Path, session: &str) -> Option<String> {
    Logs::scan(logs).continues(session)
}

/// How many logs back a lapsing claim is followed to the session that
/// goes on: a session rolls over into a new log only now and then, so a
/// chain longer than this is taken as no continuation.
pub const CONTINUED_MOST: usize = 8;

/// The sessions' logs under a logs folder, each in a folder of its working
/// folder's, `<logs>/<folder>/<time>_<session>.jsonl`, found by the
/// session each keeps in one scan of the folders, opening none: a log is
/// opened only as what it continues is asked, and only its first line is
/// read.
#[derive(Debug, Default)]
pub struct Logs {
    /// Each log found, by the session its name says it keeps.
    found: BTreeMap<String, PathBuf>,
}

impl Logs {
    /// The logs under `logs`, as they are now; none where it can't be
    /// read.
    #[must_use]
    pub fn scan(logs: &Path) -> Self {
        let found = fs::read_dir(logs)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|folder| fs::read_dir(folder.path()).ok())
            .flat_map(Iterator::flatten)
            .filter_map(|file| {
                let session = session_of_log(file.file_name().to_str()?)?.to_owned();
                Some((session, file.path()))
            })
            .collect();
        Self { found }
    }

    /// The session the log of `session` continues, as its own first line
    /// names it. None where it has no log here, where that line names
    /// another session, or where it continues none.
    #[must_use]
    pub fn continues(&self, session: &str) -> Option<String> {
        let mut first = String::new();
        io::BufRead::read_line(
            &mut io::BufReader::new(fs::File::open(self.found.get(session)?).ok()?),
            &mut first,
        )
        .ok()?;
        let header: serde_json::Value = serde_json::from_str(&first).ok()?;
        (header.get("id")?.as_str()? == session)
            .then(|| header.get("parentSession")?.as_str().map(parent_of))
            .flatten()
    }

    /// What the log of each of `sessions` continues, as
    /// [`Self::continues`] tells it, leaving out each that continues none.
    #[must_use]
    pub fn continued(&self, sessions: &[&str]) -> BTreeMap<String, String> {
        sessions
            .iter()
            .filter_map(|session| Some(((*session).to_owned(), self.continues(session)?)))
            .collect()
    }
}

/// The session a log file named `name` holds, as pi names it:
/// `<time>_<session>.jsonl`.
fn session_of_log(name: &str) -> Option<&str> {
    let (_, session) = name.strip_suffix(".jsonl")?.split_once('_')?;
    Some(session)
}

/// The session a log's `parentSession` names. pi writes the parent's log
/// as a path, whose file name holds its session; a bare session is taken
/// as it is.
fn parent_of(named: &str) -> String {
    Path::new(named)
        .file_name()
        .and_then(|name| session_of_log(name.to_str()?))
        .unwrap_or(named)
        .to_owned()
}

/// Whether the chain the quests' record gives agrees with the node's.
///
/// # Errors
///
/// When they name different orchestrators.
pub fn agrees(node: &Order, record: &Order) -> Result<(), String> {
    let named = |order: &Order| -> Vec<String> {
        order
            .chain
            .iter()
            .map(|link| link.orchestrator.clone())
            .collect()
    };
    if node.chain == record.chain && node.by == record.by {
        return Ok(());
    }
    Err(format!(
        "the quests' record gives {}'s chain as {}, but this node's record gives it as {}, so \
         the chain isn't believed",
        node.quest,
        named(record).join(" then "),
        named(node).join(" then ")
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;

    use super::{CONTINUED_MOST, Logs, Orchestrations, agrees, continues};
    use crate::carry::ordered::{Known, Link, Order};

    const STEWARD: Known = Known {
        session: "s-steward",
        address: "WCJQSG",
    };
    const WEIGH: Known = Known {
        session: "s-weigh",
        address: "OSPHT4",
    };

    fn at() -> jiff::Timestamp {
        "2026-11-15T17:00:00Z".parse().expect("a time")
    }

    fn quests(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// The steward heads WCJQSG's quest and orchestrates OSPHT4's; OSPHT4
    /// heads its own and orchestrates HARBOR's.
    fn nested() -> Orchestrations {
        let mut record = Orchestrations::default();
        record
            .claim(
                STEWARD,
                "QEST-20261007-WCJQSG",
                &quests(&["QEST-20261111-OSPHT4"]),
                None,
                at(),
            )
            .expect("the steward claims");
        record
            .claim(
                WEIGH,
                "QEST-20261111-OSPHT4",
                &quests(&["QEST-20261115-HARBOR"]),
                None,
                at(),
            )
            .expect("OSPHT4 claims");
        record
    }

    fn link(quest: &str, orchestrator: &str) -> Link {
        Link {
            quest: quest.to_owned(),
            orchestrator: orchestrator.to_owned(),
        }
    }

    #[test]
    fn the_chain_runs_up_what_each_orchestrator_claimed() {
        let record = nested();
        assert_eq!(
            record.chain("QEST-20261115-HARBOR", WEIGH),
            Ok(Order {
                by: "OSPHT4".to_owned(),
                quest: "QEST-20261115-HARBOR".to_owned(),
                chain: vec![link("QEST-20261115-HARBOR", "OSPHT4")],
            }),
            "the nearest orchestrator orders"
        );
        assert_eq!(
            record.chain("QEST-20261115-HARBOR", STEWARD),
            Ok(Order {
                by: "WCJQSG".to_owned(),
                quest: "QEST-20261115-HARBOR".to_owned(),
                chain: vec![
                    link("QEST-20261115-HARBOR", "OSPHT4"),
                    link("QEST-20261111-OSPHT4", "WCJQSG"),
                ],
            }),
            "and so does the one above it"
        );
    }

    #[test]
    fn a_session_no_orchestrator_claimed_above_it_orders_nothing() {
        let record = nested();
        assert_eq!(
            record.chain("QEST-20261111-OSPHT4", WEIGH),
            Err(
                "OSPHT4 doesn't orchestrate QEST-20261111-OSPHT4 by this node's record, so it \
                 can't order its carry: only WCJQSG's session could"
                    .to_owned()
            ),
            "a quest's own session doesn't order its carry"
        );
        assert_eq!(
            record.chain("QEST-20261115-ELSEWH", STEWARD),
            Err(
                "no orchestrator has told this node it orchestrates QEST-20261115-ELSEWH, so \
                 only the person carries it"
                    .to_owned()
            ),
        );
    }

    #[test]
    fn a_quest_another_session_holds_isnt_taken() {
        let mut record = nested();
        let before = record.clone();
        let helper = Known {
            session: "s-helper",
            address: "HARBOR",
        };
        assert_eq!(
            record.claim(
                helper,
                "QEST-20261115-HARBOR",
                &quests(&["QEST-20261111-OSPHT4"]),
                None,
                at(),
            ),
            Err(
                "WCJQSG's session already orchestrates QEST-20261111-OSPHT4 by this node's \
                 record, so HARBOR can't: only that session hands it on, or the person"
                    .to_owned()
            ),
            "a session claiming its own orchestrator's quest is refused"
        );
        assert_eq!(
            record.claim(helper, "QEST-20261007-WCJQSG", &[], None, at(),),
            Err(
                "WCJQSG's session already heads QEST-20261007-WCJQSG by this node's record, so \
                 HARBOR can't: only that session hands it on, or the person"
                    .to_owned()
            ),
            "and so is one claiming to head another's quest"
        );
        assert_eq!(record, before, "a refusal changes nothing");
    }

    #[test]
    fn an_orchestrator_says_its_quests_again_and_releases_them() {
        let mut record = nested();
        record
            .claim(
                STEWARD,
                "QEST-20261007-WCJQSG",
                &quests(&["QEST-20261111-OSPHT4", "QEST-20261115-5HJZ8Z"]),
                None,
                at(),
            )
            .expect("the steward says its quests again");
        assert_eq!(
            record.sessions.get("s-steward").map(|claim| &claim.quests),
            Some(&BTreeSet::from([
                "QEST-20261111-OSPHT4".to_owned(),
                "QEST-20261115-5HJZ8Z".to_owned()
            ])),
            "in place of what it said before"
        );
        record.release("s-weigh");
        assert!(
            record.chain("QEST-20261115-HARBOR", STEWARD).is_err(),
            "a released orchestration leads nowhere"
        );
        let helper = Known {
            session: "s-helper",
            address: "OSPHT4",
        };
        record
            .claim(
                helper,
                "QEST-20261111-OSPHT4",
                &quests(&["QEST-20261115-HARBOR"]),
                None,
                at(),
            )
            .expect("and another session may take it up");
    }

    /// The sessions that stand, each with the address the node knows it
    /// by, where it knows one.
    fn standing(sessions: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        sessions
            .iter()
            .map(|(session, address)| ((*session).to_owned(), address.map(str::to_owned)))
            .collect()
    }

    /// What each log continues, as `(session, continued)` pairs say.
    fn logs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(session, continued)| ((*session).to_owned(), (*continued).to_owned()))
            .collect()
    }

    /// Lapses `record` as `standing` and the logs' `continued` say,
    /// answering whether any log was read.
    fn lapsed(
        record: &mut Orchestrations,
        standing: &BTreeMap<String, Option<String>>,
        continued: &BTreeMap<String, String>,
    ) -> bool {
        let read = Cell::new(false);
        record.lapse(standing, || {
            read.set(true);
            |session: &str| continued.get(session).cloned()
        });
        read.get()
    }

    #[test]
    fn a_claim_lapses_once_its_session_has_ended() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[("s-weigh", Some("OSPHT4"))]),
            &BTreeMap::new(),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "only what a standing session claimed is kept"
        );
    }

    #[test]
    fn no_log_is_read_where_no_claim_lapses() {
        let mut record = nested();
        let before = record.clone();
        assert!(
            !lapsed(
                &mut record,
                &standing(&[("s-steward", Some("WCJQSG")), ("s-weigh", Some("OSPHT4"))]),
                &logs(&[("s-weigh", "s-steward")]),
            ),
            "every claim's session stands, so no log is read"
        );
        assert_eq!(record, before, "and nothing changes");
    }

    #[test]
    fn a_lapsing_claim_passes_to_the_session_whose_log_continues_it() {
        let mut record = nested();
        let was = record.sessions.get("s-steward").cloned().expect("claimed");
        assert!(lapsed(
            &mut record,
            &standing(&[("s-steward-2", Some("WCJQSG")), ("s-weigh", Some("OSPHT4"))]),
            &logs(&[("s-steward-2", "s-steward")]),
        ));
        assert!(
            !record.sessions.contains_key("s-steward"),
            "the ended session holds nothing"
        );
        assert_eq!(
            record.sessions.get("s-steward-2"),
            Some(&was),
            "its continuation holds its quests, what it heads and its address"
        );
        let rolled = Known {
            session: "s-steward-2",
            address: "WCJQSG",
        };
        assert_eq!(
            record
                .chain("QEST-20261115-HARBOR", rolled)
                .map(|order| order.by),
            Ok("WCJQSG".to_owned()),
            "so the chain still leads to it"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_through_a_log_that_never_claimed() {
        let mut record = nested();
        let was = record.sessions.get("s-steward").cloned().expect("claimed");
        lapsed(
            &mut record,
            &standing(&[("s-steward-3", None), ("s-weigh", Some("OSPHT4"))]),
            &logs(&[("s-steward-3", "s-steward-2"), ("s-steward-2", "s-steward")]),
        );
        assert_eq!(
            record.sessions.get("s-steward-3"),
            Some(&was),
            "the gap, which neither claimed nor stands, passes it on, and where the node \
             knows no address the claim keeps its own"
        );
        assert_eq!(record.sessions.len(), 2, "and none is held twice");
    }

    #[test]
    fn a_lapsing_claim_takes_the_address_its_continuation_goes_by() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[("s-steward-2", Some("KESTRL")), ("s-weigh", None)]),
            &logs(&[("s-steward-2", "s-steward")]),
        );
        assert_eq!(
            record
                .sessions
                .get("s-steward-2")
                .map(|claim| claim.address.as_str()),
            Some("KESTRL")
        );
    }

    #[test]
    fn a_continuation_too_long_takes_nothing() {
        let mut record = nested();
        let mut pairs: Vec<(String, String)> = (0..CONTINUED_MOST)
            .map(|at| (format!("s-roll-{}", at + 1), format!("s-roll-{at}")))
            .collect();
        pairs.push(("s-roll-0".to_owned(), "s-steward".to_owned()));
        let continued: BTreeMap<String, String> = pairs.into_iter().collect();
        let last = format!("s-roll-{CONTINUED_MOST}");
        lapsed(
            &mut record,
            &standing(&[(&last, None), ("s-weigh", None)]),
            &continued,
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "a walk deeper than {CONTINUED_MOST} logs is given up"
        );
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[("s-roll-1", None), ("s-weigh", None)]),
            &continued,
        );
        assert!(
            record.sessions.contains_key("s-roll-1"),
            "though a shorter one is walked"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_to_no_unrelated_standing_session() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[
                ("s-helper", Some("HARBOR")),
                ("s-fork", Some("WCJQSG")),
                ("s-weigh", Some("OSPHT4")),
            ]),
            &logs(&[("s-helper", "s-elsewhere"), ("s-fork", "s-weigh")]),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "neither a session continuing another, nor one forked from a standing one, takes it"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_to_the_nearest_standing_continuation() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[
                ("s-steward-2", None),
                ("s-steward-2-fork", None),
                ("s-weigh", None),
            ]),
            &logs(&[
                ("s-steward-2-fork", "s-steward-2"),
                ("s-steward-2", "s-steward"),
            ]),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-steward-2", "s-weigh"],
            "a fork of the standing continuation doesn't vie with it"
        );
    }

    #[test]
    fn a_lapsing_claim_two_sessions_continue_passes_to_neither() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[
                ("s-steward-2", None),
                ("s-steward-fork", None),
                ("s-weigh", None),
            ]),
            &logs(&[
                ("s-steward-2", "s-steward"),
                ("s-steward-fork", "s-steward"),
            ]),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "which of the two goes on can't be told, so it lapses"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_over_a_continuation_that_claimed_its_own() {
        let mut record = nested();
        record
            .claim(
                Known {
                    session: "s-steward-2",
                    address: "WCJQSG",
                },
                "QEST-20261115-TERNXX",
                &[],
                None,
                at(),
            )
            .expect("it claims");
        let own = record.sessions.get("s-steward-2").cloned();
        lapsed(
            &mut record,
            &standing(&[("s-steward-2", None), ("s-weigh", None)]),
            &logs(&[("s-steward-2", "s-steward")]),
        );
        assert_eq!(
            record.sessions.get("s-steward-2").cloned(),
            own,
            "what a session said itself stands over what it continues"
        );
        assert!(!record.sessions.contains_key("s-steward"));
    }

    #[test]
    fn the_person_frees_a_quest_whoever_holds_it() {
        let mut record = nested();
        assert_eq!(
            record.free("QEST-20261115-HARBOR"),
            Ok("OSPHT4".to_owned()),
            "a quest orchestrated is freed from its orchestrator"
        );
        assert_eq!(
            record
                .sessions
                .get("s-weigh")
                .map(|claim| claim.quests.len()),
            Some(0),
            "which keeps heading its own"
        );
        assert_eq!(
            record.free("QEST-20261007-WCJQSG"),
            Ok("WCJQSG".to_owned()),
            "and a quest headed frees all its session held"
        );
        assert!(
            !record.sessions.contains_key("s-steward"),
            "so its quests hang from nothing"
        );
        assert_eq!(
            record.free("QEST-20261115-HARBOR"),
            Err(
                "no session holds QEST-20261115-HARBOR by this node's record, so nothing is \
                 freed"
                    .to_owned()
            ),
        );
    }

    #[test]
    fn a_continued_session_takes_on_its_claims() {
        let mut record = nested();
        let rolled = Known {
            session: "s-weigh-2",
            address: "OSPHT4",
        };
        record
            .claim(
                rolled,
                "QEST-20261111-OSPHT4",
                &quests(&["QEST-20261115-HARBOR"]),
                Some("s-weigh"),
                at(),
            )
            .expect("the session its log continues hands them on");
        assert!(
            !record.sessions.contains_key("s-weigh"),
            "the old one holds nothing"
        );
        assert_eq!(
            record
                .chain("QEST-20261115-HARBOR", rolled)
                .map(|order| order.by),
            Ok("OSPHT4".to_owned()),
        );
    }

    #[test]
    fn the_record_is_kept_whole_and_read_back() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("mycelium").join("orchestrations.json");
        assert_eq!(
            Orchestrations::read(&file).ok(),
            Some(Orchestrations::default()),
            "none kept yet reads as none"
        );
        let record = nested();
        record.keep(&file).expect("it is kept");
        assert_eq!(Orchestrations::read(&file).ok(), Some(record));
        fs::write(&file, "{\"sessions\": 3}").expect("written");
        assert!(
            Orchestrations::read(&file).is_err(),
            "a record that can't be read proves nothing"
        );
    }

    #[test]
    fn a_log_names_the_session_it_continues() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let folder = logs.path().join("--home-joel-src-mycelium--");
        fs::create_dir_all(&folder).expect("made");
        fs::write(
            folder.join("2026-11-15T15-23-41-665Z_s-weigh-2.jsonl"),
            "{\"type\":\"session\",\"version\":3,\"id\":\"s-weigh-2\",\
             \"parentSession\":\"s-weigh\"}\n{\"type\":\"message\"}\n",
        )
        .expect("written");
        fs::write(
            folder.join("2026-11-15T14-00-00-000Z_s-steward.jsonl"),
            "{\"type\":\"session\",\"version\":3,\"id\":\"s-steward\"}\n",
        )
        .expect("written");
        assert_eq!(
            continues(logs.path(), "s-weigh-2"),
            Some("s-weigh".to_owned())
        );
        assert_eq!(
            continues(logs.path(), "s-steward"),
            None,
            "one that continues none"
        );
        assert_eq!(
            continues(logs.path(), "s-nobody"),
            None,
            "nor one with no log"
        );
    }

    #[test]
    fn a_log_names_its_parent_by_the_path_pi_writes() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let folder = logs.path().join("--home-joel-src-mycelium--");
        fs::create_dir_all(&folder).expect("made");
        let parent =
            folder.join("2026-10-11T22-41-54-195Z_01a12d21-3813-76ee-b875-6702b23f353a.jsonl");
        fs::write(
            folder.join("2026-10-23T02-03-44-444Z_01a1667f-f5bc-7ba6-9290-033e899fff34.jsonl"),
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"01a1667f-f5bc-7ba6-9290-033e899fff34\",\
                 \"timestamp\":\"2026-10-23T02:03:44.444Z\",\"cwd\":\"/home/joel/src/mycelium\",\
                 \"parentSession\":{}}}\n",
                serde_json::Value::from(parent.to_str().expect("a path in words"))
            ),
        )
        .expect("written");
        assert_eq!(
            continues(logs.path(), "01a1667f-f5bc-7ba6-9290-033e899fff34"),
            Some("01a12d21-3813-76ee-b875-6702b23f353a".to_owned()),
            "the session the parent's log file is named for"
        );
    }

    #[test]
    fn the_logs_are_scanned_once_and_read_only_where_asked() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let (one, other) = (
            logs.path().join("--home-joel-src-mycelium--"),
            logs.path().join("--home-joel--"),
        );
        for folder in [&one, &other] {
            fs::create_dir_all(folder).expect("made");
        }
        fs::write(
            one.join("2026-11-23T11-31-00-000Z_s-steward-2.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-steward-2\",\"parentSession\":\"s-steward\"}\n",
        )
        .expect("written");
        fs::write(
            other.join("2026-11-23T12-00-00-000Z_s-steward-3.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-steward-3\",\"parentSession\":\"s-steward-2\"}\n",
        )
        .expect("written");
        fs::write(
            one.join("2026-11-23T12-00-00-000Z_s-liar.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-other\",\"parentSession\":\"s-steward\"}\n",
        )
        .expect("written");
        fs::write(one.join("notes.txt"), "not a log\n").expect("written");
        let found = Logs::scan(logs.path());
        // Whatever lands after the scan isn't seen, so the folder was read
        // once, as the scan was made.
        fs::write(
            one.join("2026-11-23T13-00-00-000Z_s-later.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-later\",\"parentSession\":\"s-steward\"}\n",
        )
        .expect("written");
        assert_eq!(
            found.continues("s-steward-3"),
            Some("s-steward-2".to_owned())
        );
        assert_eq!(found.continues("s-steward-2"), Some("s-steward".to_owned()));
        assert_eq!(
            found.continues("s-liar"),
            None,
            "a log whose first line names another session continues none"
        );
        assert_eq!(found.continues("s-later"), None, "nor one not yet there");
        assert_eq!(
            found.continued(&["s-steward-3", "s-steward-2", "s-nobody"]),
            BTreeMap::from([
                ("s-steward-2".to_owned(), "s-steward".to_owned()),
                ("s-steward-3".to_owned(), "s-steward-2".to_owned()),
            ]),
            "and a set of sessions reads as one"
        );
    }

    #[test]
    fn the_quests_record_must_name_the_same_orchestrators() {
        let node = Order {
            by: "WCJQSG".to_owned(),
            quest: "QEST-20261115-HARBOR".to_owned(),
            chain: vec![
                link("QEST-20261115-HARBOR", "OSPHT4"),
                link("QEST-20261111-OSPHT4", "WCJQSG"),
            ],
        };
        assert_eq!(agrees(&node, &node.clone()), Ok(()));
        let rewritten = Order {
            by: "WCJQSG".to_owned(),
            quest: "QEST-20261115-HARBOR".to_owned(),
            chain: vec![link("QEST-20261115-HARBOR", "WCJQSG")],
        };
        assert_eq!(
            agrees(&node, &rewritten),
            Err(
                "the quests' record gives QEST-20261115-HARBOR's chain as WCJQSG, but this \
                 node's record gives it as OSPHT4 then WCJQSG, so the chain isn't believed"
                    .to_owned()
            ),
        );
    }
}
