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
pub struct Hello {
    /// Its machine's name.
    pub machine: String,
    /// Its realm.
    pub realm: String,
    /// The link's version it speaks.
    pub version: u32,
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

#[cfg(test)]
mod tests {
    use super::*;

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
