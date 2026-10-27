//! A shared file's version: how many times each machine has changed it.
//!
//! Two machines write a shared file apart, so neither's clock can order
//! their writes. Each keeps a count per machine instead, and a machine
//! adds one to its own only when it sees the file change on its disk. One
//! version is newer than another when it has seen everything the other
//! has and more; two that have each seen a change the other hasn't were
//! written apart, and both are kept.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How many times each machine has changed a file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version(BTreeMap<String, u64>);

/// How one version stands against another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Both have seen the same changes.
    Same,
    /// This one has seen every change the other has, and more.
    Newer,
    /// The other has seen every change this one has, and more.
    Older,
    /// Each has seen a change the other hasn't: written apart.
    Apart,
}

impl Version {
    /// The version after `machine` changes the file once more.
    #[must_use]
    pub fn changed_on(&self, machine: &str) -> Self {
        let mut counts = self.0.clone();
        *counts.entry(machine.to_owned()).or_default() += 1;
        Self(counts)
    }

    /// The version that has seen every change either has, as a file that
    /// settles two written apart has.
    #[must_use]
    pub fn joined(&self, other: &Self) -> Self {
        let mut counts = self.0.clone();
        for (machine, &count) in &other.0 {
            let mine = counts.entry(machine.clone()).or_default();
            *mine = (*mine).max(count);
        }
        Self(counts)
    }

    /// How this version stands against `other`.
    #[must_use]
    pub fn against(&self, other: &Self) -> Standing {
        let machines = self.0.keys().chain(other.0.keys());
        let (mut ahead, mut behind) = (false, false);
        for machine in machines {
            match self.count(machine).cmp(&other.count(machine)) {
                Ordering::Greater => ahead = true,
                Ordering::Less => behind = true,
                Ordering::Equal => {}
            }
        }
        match (ahead, behind) {
            (false, false) => Standing::Same,
            (true, false) => Standing::Newer,
            (false, true) => Standing::Older,
            (true, true) => Standing::Apart,
        }
    }

    fn count(&self, machine: &str) -> u64 {
        self.0.get(machine).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_makes_a_version_newer() {
        let first = Version::default().changed_on("optimus");
        let second = first.changed_on("equilibrius");
        assert_eq!(
            second.against(&first),
            Standing::Newer,
            "a change on top of a version is newer than it"
        );
        assert_eq!(
            first.against(&second),
            Standing::Older,
            "a version is older than a change on top of it"
        );
        assert_eq!(
            first.against(&first.clone()),
            Standing::Same,
            "a version is the same as itself"
        );
    }

    #[test]
    fn changes_made_apart_are_apart_until_joined() {
        let shared = Version::default().changed_on("optimus");
        let here = shared.changed_on("optimus");
        let there = shared.changed_on("equilibrius");
        assert_eq!(
            here.against(&there),
            Standing::Apart,
            "two changes on one version, each on its own machine, are apart"
        );
        let settled = here.joined(&there).changed_on("optimus");
        assert_eq!(
            settled.against(&here),
            Standing::Newer,
            "the settled version is newer than this machine's"
        );
        assert_eq!(
            settled.against(&there),
            Standing::Newer,
            "the settled version is newer than the other machine's"
        );
    }
}
