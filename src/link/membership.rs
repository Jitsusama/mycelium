//! Membership: which machines a node links with. A link's two ends each
//! say who they are first, and each end's membership admits the other or
//! refuses it before anything else crosses.
//!
//! A machine's identity today is what SSH proved when the link was dialled,
//! so membership checks only what each end says of itself. When links run
//! over keys of their own, membership is where a realm's enrolled keys are
//! checked, behind the same trait.

use serde::{Deserialize, Serialize};

/// What this end of a link speaks; an end refuses one that speaks another.
pub const VERSION: u32 = 1;

/// What an end of a link says of itself, first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each says on the wire what an end knows of, apart, so an older end leaves it out"
)]
pub struct Hello {
    /// Its machine's name.
    pub machine: String,
    /// Its realm.
    pub realm: String,
    /// The link's version it speaks.
    pub version: u32,
    /// Whether it says what its machine is, and keeps what the other end
    /// says; an end older than this does neither, and would end a link
    /// that sent it.
    #[serde(default)]
    pub facts: bool,
    /// Whether it carries starts, asked of its machine's node and
    /// answered; an end older than this does neither, and would end a
    /// link that asked.
    #[serde(default)]
    pub starts: bool,
    /// Whether it carries steers, writing them into its sessions' inboxes
    /// and carrying talks' replies back; an end older than this does
    /// neither, and would end a link that sent one.
    #[serde(default)]
    pub steers: bool,
    /// Whether its way to the other end's machine can release that machine
    /// once it hands over, as a provider's program can a machine it lent;
    /// only an end that dialled one says so, and an end older than this
    /// is never handed over to.
    #[serde(default)]
    pub releases: bool,
}

/// Which machines a node links with.
pub trait Membership: Send + Sync + 'static {
    /// What this end says of itself.
    fn hello(&self) -> Hello;

    /// Admits the other end, or says why not.
    fn admit(&self, other: &Hello) -> Result<(), String>;
}

/// A realm: every machine of the same realm, speaking the same version,
/// is admitted, and a machine never links with itself.
#[derive(Debug, Clone)]
pub struct Realm {
    /// The realm's name.
    pub name: String,
    /// This machine's name.
    pub machine: String,
}

impl Membership for Realm {
    fn hello(&self) -> Hello {
        Hello {
            machine: self.machine.clone(),
            realm: self.name.clone(),
            version: VERSION,
            facts: true,
            starts: true,
            steers: true,
            releases: false,
        }
    }

    fn admit(&self, other: &Hello) -> Result<(), String> {
        if other.version != VERSION {
            return Err(format!(
                "{} speaks link version {}, not {VERSION}",
                other.machine, other.version
            ));
        }
        if other.realm != self.name {
            return Err(format!(
                "{} is in realm {}, not {}",
                other.machine, other.realm, self.name
            ));
        }
        if other.machine == self.machine {
            return Err(format!(
                "{} is this machine, which never links with itself",
                other.machine
            ));
        }
        Ok(())
    }
}

/// A membership whose far end is proven to be one machine, as the key a
/// link was let in by is that machine's: what `by` admits, and only if it
/// says it is `far`.
#[derive(Debug, Clone)]
pub struct Proven<M> {
    /// What it otherwise admits by.
    pub by: M,
    /// The machine the far end is proven to be.
    pub far: String,
}

impl<M: Membership> Membership for Proven<M> {
    fn hello(&self) -> Hello {
        self.by.hello()
    }

    fn admit(&self, other: &Hello) -> Result<(), String> {
        self.by.admit(other)?;
        if other.machine == self.far {
            Ok(())
        } else {
            Err(format!(
                "this link came in by {}'s key, so it can't be {}",
                self.far, other.machine
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proven_end_is_admitted_only_as_the_machine_its_key_is() {
        let proven = Proven {
            by: realm("work", "optimus"),
            far: "equilibrius".to_owned(),
        };
        assert_eq!(
            proven.admit(&realm("work", "equilibrius").hello()),
            Ok(()),
            "the machine its key is"
        );
        assert_eq!(
            proven.admit(&realm("work", "penelope").hello()),
            Err("this link came in by equilibrius's key, so it can't be penelope".to_owned()),
            "another machine saying it is itself"
        );
        assert!(
            proven
                .admit(&realm("personal", "equilibrius").hello())
                .is_err(),
            "and what its realm refuses still is"
        );
    }

    fn realm(name: &str, machine: &str) -> Realm {
        Realm {
            name: name.to_owned(),
            machine: machine.to_owned(),
        }
    }

    #[test]
    fn a_machine_of_the_same_realm_is_admitted() {
        let optimus = realm("work", "optimus");
        assert_eq!(optimus.admit(&realm("work", "equilibrius").hello()), Ok(()));
    }

    #[test]
    fn another_realm_this_machine_and_another_version_are_refused() {
        let optimus = realm("work", "optimus");
        for other in [
            realm("personal", "penelope").hello(),
            realm("work", "optimus").hello(),
            Hello {
                version: VERSION + 1,
                ..realm("work", "equilibrius").hello()
            },
        ] {
            assert!(
                optimus.admit(&other).is_err(),
                "{other:?} should be refused"
            );
        }
    }
}
