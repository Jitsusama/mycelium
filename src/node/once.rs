//! A start asked again is the one already asked. An asker that hears no
//! answer in time can't tell a start that never came from one still
//! landing, so it asks again; under the name it gave the first, the node
//! answers that one, landing or landed, rather than starting and counting
//! another (KP0L6A, 19 November: a start answered after the harness's 45
//! seconds landed, and the retry started a second session).
//!
//! A name is the asker's own: the same name from another asker, by who
//! the node judged it and the cell it counts toward, is another start.
//! One that landed is answered for an hour, as long as a start counts; one
//! that failed or was refused is let go, so asking again tries afresh.
//! One sent to another machine that answered nothing may still land
//! there, so for that hour an ask again goes back to that machine, under
//! the same name, rather than being placed and counted again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use crate::wire::{By, Started};

/// The longest name an asker may give a start, in bytes.
pub const NAME_MOST: usize = 64;

/// How long a start that landed, or went unanswered on another machine,
/// is held to its name, in milliseconds: the hour it counts for.
const LANDED_FOR: i64 = 3_600_000;

/// The most names held at once: past it, the oldest ended are let go
/// first, so asks under fresh names without end fill no memory.
const HELD_MOST: usize = 4096;

/// The hex digits of a name's hash that a name sent on keeps.
const HASHED: usize = 16;

/// A start's name as the node knows it: the asker's, whether the person
/// or an agent asked it, and the cell it counts toward. Not the agent's
/// fence, which may come out otherwise when it asks again, as a claim
/// above reads or not, so an ask again joins the first as the book finds
/// it; the cell and the name tell one agent's asks apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Name {
    by: String,
    cell: Option<String>,
    said: String,
}

impl Name {
    /// The name `name`, as `by` asked it for the cell `cell`.
    ///
    /// # Errors
    ///
    /// When the name is empty or longer than [`NAME_MOST`].
    pub fn of(by: By, cell: Option<&str>, name: &str) -> Result<Self, String> {
        if name.is_empty() || name.len() > NAME_MOST {
            return Err(format!(
                "a start's name is 1 to {NAME_MOST} bytes, and this one is {}",
                name.len()
            ));
        }
        // Every agent is spelled as a strict one was before, the fence
        // most agents are judged in, so the name `there` sends for most
        // of their asks stays what an older build sent.
        let by = match by {
            By::Person => by.to_string(),
            By::Agent(_) => By::Agent(crate::declared::Kind::Strict).to_string(),
        };
        Ok(Self {
            by,
            cell: cell.map(str::to_owned),
            said: name.to_owned(),
        })
    }

    /// The name it goes under to the machine asked to start it, from
    /// `here`: one the far node can't confuse with another asker's, since
    /// every start it hears from here comes from this node. Its hash is
    /// kept whole and a long machine name cut short, so no two askers'
    /// names meet however long `here` is; the hash is blake3's, the same
    /// from every build, so a node updated between an ask and its retry
    /// sends the same name. The one exception is an agent judged in a
    /// fence other than strict, named by a build before every agent was
    /// spelled alike, whose name changes once, across that update.
    #[must_use]
    pub fn there(&self, here: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        for part in [
            here,
            &self.by,
            self.cell.as_deref().unwrap_or_default(),
            &self.said,
        ] {
            hasher.update(&part.len().to_le_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.update(&[u8::from(self.cell.is_some())]);
        let hash = &hasher.finalize().to_hex()[..HASHED];
        let room = NAME_MOST - HASHED - 1;
        let mut cut = here.len().min(room);
        while !here.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}-{hash}", &here[..cut])
    }
}

/// What a start comes to, as those who joined it hear.
type Outcome = Result<Started, String>;

enum Held {
    /// Still landing: its outcome comes on this, and this many asks
    /// joined it.
    Landing {
        told: watch::Receiver<Option<Outcome>>,
        joined: u32,
    },
    /// Landed at this time, in milliseconds since the epoch.
    Landed { at: i64, started: Started },
    /// Sent to this machine, which answered nothing, at this time.
    Unanswered { at: i64, machine: String },
}

impl Held {
    /// When it ended, if it has.
    fn ended(&self) -> Option<i64> {
        match self {
            Self::Landing { .. } => None,
            Self::Landed { at, .. } | Self::Unanswered { at, .. } => Some(*at),
        }
    }
}

#[derive(Default)]
struct Book {
    held: HashMap<Name, Held>,
    /// The names sent on to another machine whose link answered nothing,
    /// with that machine and whether the ask was sent before it did.
    unanswered: HashMap<String, (String, bool)>,
}

/// The starts asked under a name, landing or landed.
#[derive(Default)]
pub struct Onces(Mutex<Book>);

/// What asking under a name comes to.
pub enum Joined {
    /// The one start under it landed already, as this says.
    Landed(Started),
    /// The one start under it is landing: wait on [`Joining::outcome`].
    Landing(Joining),
    /// None is under it: this ask starts it, and says how it ends.
    Leads(Lead),
    /// The one under it went to `machine`, which answered nothing: this
    /// ask asks it there again, under the same name, counting nothing
    /// more, and says how that ends.
    Resends { machine: String, lead: Lead },
}

/// An ask waiting on the start under its name.
pub struct Joining(watch::Receiver<Option<Outcome>>);

impl Joining {
    /// How the start it joined ends.
    ///
    /// # Errors
    ///
    /// Why it didn't start, as that start says, or that it stopped.
    pub async fn outcome(mut self) -> Outcome {
        match self.0.wait_for(Option::is_some).await {
            Ok(ended) => ended.clone().unwrap_or_else(unreachable_outcome),
            Err(_) => Err("the start it joined stopped before it ended".to_owned()),
        }
    }
}

/// What a start that ended with no outcome says; never, as `wait_for`
/// waits for one.
fn unreachable_outcome() -> Outcome {
    Err("the start it joined ended saying nothing".to_owned())
}

/// The ask starting a start under a name, which says how it ended.
#[must_use = "a start led under a name says how it ended"]
pub struct Lead {
    onces: Arc<Onces>,
    name: Name,
    /// The name it goes under to another machine.
    there: String,
    /// Whether it asks again a start sent before, which answered nothing.
    again: bool,
    told: watch::Sender<Option<Outcome>>,
    ended: bool,
}

impl Lead {
    /// Says how it ended, at `at`, to every ask that joined it, answering
    /// how many did. One that started is answered to its name for an
    /// hour. One sent to another machine that answered nothing, or asked
    /// there again and not reaching it, may still land there, so it is
    /// held for the hour to be asked there again. Any other is let go, so
    /// asking again tries afresh.
    pub fn ends(mut self, at: i64, outcome: &Outcome) -> u32 {
        self.ended = true;
        let mut book = self.onces.lock();
        let unanswered = book.unanswered.remove(&self.there);
        let joined = match book.held.remove(&self.name) {
            Some(Held::Landing { joined, .. }) => joined,
            _ => 0,
        };
        match (outcome, unanswered) {
            (Ok(started), _) => {
                book.held.insert(
                    self.name.clone(),
                    Held::Landed {
                        at,
                        started: started.clone(),
                    },
                );
            }
            (Err(_), Some((machine, sent))) if sent || self.again => {
                book.held
                    .insert(self.name.clone(), Held::Unanswered { at, machine });
            }
            (Err(_), _) => {}
        }
        drop(book);
        // Nobody may have joined it, which is no fault.
        let _ = self.told.send(Some(outcome.clone()));
        joined
    }
}

impl Drop for Lead {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        // A lead dropped before it ended, as a stopped start drops it,
        // lets its name go, so those that joined hear it stopped and an
        // ask again tries afresh.
        let mut book = self.onces.lock();
        book.unanswered.remove(&self.there);
        if matches!(book.held.get(&self.name), Some(Held::Landing { .. })) {
            book.held.remove(&self.name);
        }
    }
}

impl Onces {
    /// Asks under `name` at `at`, on the node of the machine `here`: the
    /// start under it, landing or landed, or this ask's lead of a new one.
    pub fn join(self: &Arc<Self>, name: Name, here: &str, at: i64) -> Joined {
        let mut book = self.lock();
        book.held
            .retain(|_, one| one.ended().is_none_or(|ended| at - ended < LANDED_FOR));
        let again = match book.held.get_mut(&name) {
            Some(Held::Landed { started, .. }) => return Joined::Landed(started.clone()),
            Some(Held::Landing { told, joined }) => {
                *joined = joined.saturating_add(1);
                return Joined::Landing(Joining(told.clone()));
            }
            Some(Held::Unanswered { machine, .. }) => Some(machine.clone()),
            None => None,
        };
        while book.held.len() >= HELD_MOST {
            let oldest = book
                .held
                .iter()
                .filter_map(|(name, one)| one.ended().map(|at| (at, name.clone())))
                .min_by_key(|(at, _)| *at)
                .map(|(_, name)| name);
            match oldest {
                Some(oldest) => drop(book.held.remove(&oldest)),
                None => break,
            }
        }
        let (told, hears) = watch::channel(None);
        book.held.insert(
            name.clone(),
            Held::Landing {
                told: hears,
                joined: 0,
            },
        );
        drop(book);
        let lead = Lead {
            onces: Arc::clone(self),
            there: name.there(here),
            name,
            again: again.is_some(),
            told,
            ended: false,
        };
        match again {
            Some(machine) => Joined::Resends { machine, lead },
            None => Joined::Leads(lead),
        }
    }

    /// Says the start sent to `machine` under the name `there` had no
    /// answer from it, `sent` saying whether it was asked before the link
    /// failed, so its lead holds the name to ask there again.
    pub fn unanswered(&self, there: &str, machine: &str, sent: bool) {
        self.lock()
            .unanswered
            .insert(there.to_owned(), (machine.to_owned(), sent));
    }

    fn lock(&self) -> MutexGuard<'_, Book> {
        // Each change is one insert or remove, so a panic leaves the map
        // whole.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declared::Kind;

    const NOW: i64 = 1_800_000_000_000;

    const HERE: &str = "osprey";

    fn started(held: &str) -> Started {
        Started {
            machine: "skiff".to_owned(),
            held: held.to_owned(),
            folder: "/home/jsama".into(),
            attach: vec!["mycelium".to_owned(), "attach".to_owned(), held.to_owned()],
        }
    }

    fn agent(cell: &str, name: &str) -> Name {
        Name::of(By::Agent(Kind::Strict), Some(cell), name).expect("a name")
    }

    fn leads(joined: Joined) -> Lead {
        match joined {
            Joined::Leads(lead) => lead,
            _ => panic!("nothing was under the name"),
        }
    }

    fn resends(joined: Joined) -> (String, Lead) {
        match joined {
            Joined::Resends { machine, lead } => (machine, lead),
            _ => panic!("it isn't asked again where it went"),
        }
    }

    #[tokio::test]
    async fn an_ask_under_a_landing_name_hears_how_that_start_ends() {
        let onces = Arc::new(Onces::default());
        let lead = leads(onces.join(agent("OSPREY", "01J"), HERE, NOW));
        let Joined::Landing(joining) = onces.join(agent("OSPREY", "01J"), HERE, NOW + 1) else {
            panic!("the second ask joins the first");
        };
        assert_eq!(lead.ends(NOW + 2, &Ok(started("h-1"))), 1, "one joined");
        assert_eq!(joining.outcome().await, Ok(started("h-1")));
        let Joined::Landed(landed) = onces.join(agent("OSPREY", "01J"), HERE, NOW + 3) else {
            panic!("one asked after it landed is answered it");
        };
        assert_eq!(landed, started("h-1"));
    }

    #[tokio::test]
    async fn a_name_from_another_cell_or_after_a_failure_or_an_hour_is_a_start_afresh() {
        let onces = Arc::new(Onces::default());
        let lead = leads(onces.join(agent("OSPREY", "01J"), HERE, NOW));
        let theirs = leads(onces.join(agent("KESTRL", "01J"), HERE, NOW));
        let persons = Name::of(By::Person, None, "01J").expect("a name");
        let person = leads(onces.join(persons, HERE, NOW));
        lead.ends(NOW, &Err("it didn't start".to_owned()));
        let again = leads(onces.join(agent("OSPREY", "01J"), HERE, NOW + 1));
        again.ends(NOW + 1, &Ok(started("h-2")));
        assert!(matches!(
            onces.join(agent("OSPREY", "01J"), HERE, NOW + 1 + LANDED_FOR),
            Joined::Leads(_)
        ));
        drop((theirs, person));
    }

    #[tokio::test]
    async fn a_lead_dropped_before_it_ends_lets_its_name_go() {
        let onces = Arc::new(Onces::default());
        let lead = leads(onces.join(agent("OSPREY", "01J"), HERE, NOW));
        let Joined::Landing(joining) = onces.join(agent("OSPREY", "01J"), HERE, NOW) else {
            panic!("joined");
        };
        drop(lead);
        assert!(
            joining.outcome().await.is_err(),
            "it hears the start stopped"
        );
        assert!(matches!(
            onces.join(agent("OSPREY", "01J"), HERE, NOW),
            Joined::Leads(_)
        ));
    }

    #[test]
    fn a_lead_that_ended_leaves_the_next_leads_start_alone() {
        let onces = Arc::new(Onces::default());
        let failed = leads(onces.join(agent("OSPREY", "01J"), HERE, NOW));
        let ended = failed.ends(NOW, &Err("it didn't start".to_owned()));
        assert_eq!(ended, 0);
        let next = leads(onces.join(agent("OSPREY", "01J"), HERE, NOW + 1));
        assert!(
            matches!(
                onces.join(agent("OSPREY", "01J"), HERE, NOW + 2),
                Joined::Landing(_)
            ),
            "the next lead's start is still under its name"
        );
        drop(next);
    }

    // A start sent on over a link that times out may still land there:
    // asked again, it goes back there under the same name, not placed and
    // counted afresh.
    #[test]
    fn a_start_another_machine_answered_nothing_is_asked_there_again() {
        let onces = Arc::new(Onces::default());
        let name = agent("OSPREY", "01J");
        let lead = leads(onces.join(name.clone(), HERE, NOW));
        onces.unanswered(&name.there(HERE), "kestrel", true);
        lead.ends(NOW, &Err("kestrel didn't answer within 30s".to_owned()));

        let (machine, again) = resends(onces.join(name.clone(), HERE, NOW + 1));
        assert_eq!(machine, "kestrel", "it is asked where it went");
        onces.unanswered(&name.there(HERE), "kestrel", false);
        again.ends(NOW + 1, &Err("no link reaches kestrel now".to_owned()));

        let (_, again) = resends(onces.join(name.clone(), HERE, NOW + 2));
        again.ends(NOW + 2, &Err("it didn't start".to_owned()));
        assert!(
            matches!(onces.join(name.clone(), HERE, NOW + 3), Joined::Leads(_)),
            "once kestrel says it didn't start, asking again starts afresh"
        );

        let other = agent("OSPREY", "01K");
        let lead = leads(onces.join(other.clone(), HERE, NOW));
        onces.unanswered(&other.there(HERE), "kestrel", false);
        lead.ends(NOW, &Err("the link to kestrel has just dropped".to_owned()));
        assert!(
            matches!(onces.join(other, HERE, NOW + 1), Joined::Leads(_)),
            "one never sent there is placed afresh"
        );
    }

    #[test]
    fn a_name_is_held_to_a_bound_and_goes_elsewhere_as_this_nodes_own() {
        assert!(Name::of(By::Person, None, "").is_err());
        assert!(Name::of(By::Person, None, &"x".repeat(NAME_MOST + 1)).is_err());
        let ours = agent("OSPREY", "01J");
        assert_ne!(ours.there("skiff"), agent("KESTRL", "01J").there("skiff"));
        assert_eq!(ours.there("skiff"), agent("OSPREY", "01J").there("skiff"));
        let long = "a-machine-named-far-longer-than-any-name-a-start-may-carry-at-all";
        assert!(ours.there(long).len() <= NAME_MOST);
        assert_ne!(
            ours.there(long),
            agent("KESTRL", "01J").there(long),
            "a long machine name cuts no asker's hash"
        );
    }

    // Round 2 of the judge's half: an agent's fence may come out otherwise
    // when it asks again, as a claim above reads or not or its declaration
    // moves it, and the book finds the first start whatever the fence, so
    // the name must too, or the ask again starts a second session.
    #[test]
    fn an_agents_name_is_the_same_whatever_fence_it_was_judged() {
        let judged = |by: By| Name::of(by, Some("OSPREY"), "01J").expect("a name");
        for kind in [Kind::None, Kind::Fence] {
            assert_eq!(
                judged(By::Agent(kind)),
                agent("OSPREY", "01J"),
                "{kind:?} and strict are one asker's name"
            );
            assert_eq!(
                judged(By::Agent(kind)).there("skiff"),
                agent("OSPREY", "01J").there("skiff"),
                "and go elsewhere as one: {kind:?}"
            );
        }
        assert_ne!(
            judged(By::Person),
            agent("OSPREY", "01J"),
            "but the person's is never an agent's"
        );
    }
}
