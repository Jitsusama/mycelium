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

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use crate::wire::{By, Started};

/// The longest name an asker may give a start, in bytes.
pub const NAME_MOST: usize = 64;

/// How long a start that landed is answered to its name, in milliseconds:
/// the hour it counts for.
const LANDED_FOR: i64 = 3_600_000;

/// The most names held at once: past it, the oldest landed are let go
/// first, so asks under fresh names without end fill no memory.
const HELD_MOST: usize = 4096;

/// A start's name as the node knows it: the asker's, who it was judged,
/// and the cell it counts toward.
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
        Ok(Self {
            by: by.to_string(),
            cell: cell.map(str::to_owned),
            said: name.to_owned(),
        })
    }

    /// The name it goes under to the machine `there` asked to start it,
    /// from `here`: one the far node can't confuse with another asker's,
    /// since every start it hears from here comes from this node.
    #[must_use]
    pub fn there(&self, here: &str) -> String {
        let mut hasher = DefaultHasher::new();
        (here, &self.by, &self.cell, &self.said).hash(&mut hasher);
        format!("{here}-{:016x}", hasher.finish())
            .chars()
            .take(NAME_MOST)
            .collect()
    }
}

/// What a start comes to, as those who joined it hear.
type Outcome = Result<Started, String>;

enum Held {
    /// Still landing: its outcome comes on this.
    Landing(watch::Receiver<Option<Outcome>>),
    /// Landed at this time, in milliseconds since the epoch.
    Landed { at: i64, started: Started },
}

/// The starts asked under a name, landing or landed.
#[derive(Default)]
pub struct Onces(Mutex<HashMap<Name, Held>>);

/// What asking under a name comes to.
pub enum Joined {
    /// The one start under it landed already, as this says.
    Landed(Started),
    /// The one start under it is landing: wait on [`Joining::outcome`].
    Landing(Joining),
    /// None is under it: this ask starts it, and says how it ends.
    Leads(Lead),
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
    told: watch::Sender<Option<Outcome>>,
}

impl Lead {
    /// Says how it ended, at `at`, to every ask that joined it: one that
    /// started is answered to its name for an hour; one that didn't is let
    /// go, so asking again tries afresh.
    pub fn ends(self, at: i64, outcome: &Outcome) {
        let mut held = self.onces.lock();
        match outcome {
            Ok(started) => {
                held.insert(
                    self.name.clone(),
                    Held::Landed {
                        at,
                        started: started.clone(),
                    },
                );
            }
            Err(_) => {
                held.remove(&self.name);
            }
        }
        drop(held);
        // Nobody may have joined it, which is no fault.
        let _ = self.told.send(Some(outcome.clone()));
    }
}

impl Drop for Lead {
    fn drop(&mut self) {
        // A lead dropped before it ended, as a stopped start drops it,
        // lets its name go, so those that joined hear it stopped and an
        // ask again tries afresh.
        let mut held = self.onces.lock();
        if matches!(held.get(&self.name), Some(Held::Landing(_))) {
            held.remove(&self.name);
        }
    }
}

impl Onces {
    /// Asks under `name` at `at`: the start under it, landing or landed,
    /// or this ask's lead of a new one.
    pub fn join(self: &Arc<Self>, name: Name, at: i64) -> Joined {
        let mut held = self.lock();
        held.retain(|_, one| match one {
            Held::Landing(_) => true,
            Held::Landed { at: landed, .. } => at - *landed < LANDED_FOR,
        });
        match held.get(&name) {
            Some(Held::Landed { started, .. }) => return Joined::Landed(started.clone()),
            Some(Held::Landing(told)) => return Joined::Landing(Joining(told.clone())),
            None => {}
        }
        while held.len() >= HELD_MOST {
            let oldest = held
                .iter()
                .filter_map(|(name, one)| match one {
                    Held::Landed { at, .. } => Some((*at, name.clone())),
                    Held::Landing(_) => None,
                })
                .min_by_key(|(at, _)| *at)
                .map(|(_, name)| name);
            match oldest {
                Some(oldest) => drop(held.remove(&oldest)),
                None => break,
            }
        }
        let (told, hears) = watch::channel(None);
        held.insert(name.clone(), Held::Landing(hears));
        drop(held);
        Joined::Leads(Lead {
            onces: Arc::clone(self),
            name,
            told,
        })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<Name, Held>> {
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

    #[tokio::test]
    async fn an_ask_under_a_landing_name_hears_how_that_start_ends() {
        let onces = Arc::new(Onces::default());
        let lead = leads(onces.join(agent("OSPREY", "01J"), NOW));
        let Joined::Landing(joining) = onces.join(agent("OSPREY", "01J"), NOW + 1) else {
            panic!("the second ask joins the first");
        };
        lead.ends(NOW + 2, &Ok(started("h-1")));
        assert_eq!(joining.outcome().await, Ok(started("h-1")));
        let Joined::Landed(landed) = onces.join(agent("OSPREY", "01J"), NOW + 3) else {
            panic!("one asked after it landed is answered it");
        };
        assert_eq!(landed, started("h-1"));
    }

    #[tokio::test]
    async fn a_name_from_another_cell_or_after_a_failure_or_an_hour_is_a_start_afresh() {
        let onces = Arc::new(Onces::default());
        let lead = leads(onces.join(agent("OSPREY", "01J"), NOW));
        let theirs = leads(onces.join(agent("KESTRL", "01J"), NOW));
        let persons = Name::of(By::Person, None, "01J").expect("a name");
        let person = leads(onces.join(persons, NOW));
        lead.ends(NOW, &Err("it didn't start".to_owned()));
        let again = leads(onces.join(agent("OSPREY", "01J"), NOW + 1));
        again.ends(NOW + 1, &Ok(started("h-2")));
        assert!(matches!(
            onces.join(agent("OSPREY", "01J"), NOW + 1 + LANDED_FOR),
            Joined::Leads(_)
        ));
        drop((theirs, person));
    }

    #[tokio::test]
    async fn a_lead_dropped_before_it_ends_lets_its_name_go() {
        let onces = Arc::new(Onces::default());
        let lead = leads(onces.join(agent("OSPREY", "01J"), NOW));
        let Joined::Landing(joining) = onces.join(agent("OSPREY", "01J"), NOW) else {
            panic!("joined");
        };
        drop(lead);
        assert!(
            joining.outcome().await.is_err(),
            "it hears the start stopped"
        );
        assert!(matches!(
            onces.join(agent("OSPREY", "01J"), NOW),
            Joined::Leads(_)
        ));
    }

    #[test]
    fn a_name_is_held_to_a_bound_and_goes_elsewhere_as_this_nodes_own() {
        assert!(Name::of(By::Person, None, "").is_err());
        assert!(Name::of(By::Person, None, &"x".repeat(NAME_MOST + 1)).is_err());
        let ours = agent("OSPREY", "01J");
        assert_ne!(ours.there("skiff"), agent("KESTRL", "01J").there("skiff"));
        assert_eq!(ours.there("skiff"), agent("OSPREY", "01J").there("skiff"));
        assert!(ours.there("a-machine-named-far-longer-than-most").len() <= NAME_MOST);
    }
}
