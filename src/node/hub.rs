//! The hubs of the realm's subjects, as the daemon interface holds them: a
//! record for each subject a cell declares, naming the cells a need on it
//! goes to first. A router finds a need's fit through them in a hop where
//! ties alone take several.
//!
//! A hub is made by the node's own count and grants nothing: it is where
//! a need is sent first, never a cell that may do more than another, so a
//! cell gains no power by declaring a subject or being made its hub.
//!
//! Each subject has a hub for every [`HOLDERS_A_HUB`] cells that declare
//! it, and each hub names at most [`MOST_FITS`] of them as its fits, the
//! least heard lately first, since a cell talking least has the most room
//! for a need. A cell hubs one subject, the rarest first, so a subject
//! only a few hold still gets one of them: where too few of its holders
//! are free, those hubbing fewest hub it too, since a subject with no
//! hub loses the needs only a hub would find, and one with a hub short
//! sends all its needs to fewer cells than its holders could share them
//! among.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::{daemon, networks};

/// The kind of a subject's hubs' record.
pub const KIND: &str = "hub";

/// The fields of a subject's hubs that change while it lives.
pub const LIVE: [&str; 3] = ["hub", "fits", "hubs"];

/// How many cells declaring a subject make it one more hub.
const HOLDERS_A_HUB: usize = 10;

/// The most fits a hub names.
const MOST_FITS: usize = 5;

/// How much a cell's own record of talk, a `tie:<cell>` record, says it
/// was heard lately: its messages in the last hour, and each of the day's
/// as a 24th of one, so an hour's talk outweighs a day's of the same; by
/// its cell. None for a tie between two, or a record it can't read.
#[must_use]
pub fn heard(talk: &serde_json::Value) -> Option<(String, f64)> {
    let cell = talk["key"].as_str()?.strip_prefix("tie:")?;
    if cell.contains('~') {
        return None;
    }
    let (hour, today) = (talk["hour"].as_u64()?, talk["today"].as_u64()?);
    let (hour, today) = (u32::try_from(hour).ok()?, u32::try_from(today).ok()?);
    Some((cell.to_owned(), f64::from(hour) + f64::from(today) / 24.0))
}

/// A cell that declares subjects, and how much it was heard lately.
#[derive(Debug, Clone, PartialEq)]
pub struct Holder {
    /// Its address.
    pub address: String,
    /// The subjects it declares.
    pub subjects: Vec<String>,
    /// How much it talked lately, as [`heard`] tells it: the less, the
    /// more room it has.
    pub heard: f64,
}

/// Each declared subject's hubs, by key, a subject read by
/// [`networks::subject_key`] so two cells saying it differently hold the
/// same one, as the cap counts it.
#[must_use]
pub fn records(holders: &[Holder]) -> BTreeMap<String, serde_json::Value> {
    let mut held: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for (at, holder) in holders.iter().enumerate() {
        for subject in &holder.subjects {
            held.entry(networks::subject_key(subject))
                .or_default()
                .insert(at);
        }
    }
    // Each subject's holders, least heard first, the rarest subject first.
    let mut subjects: Vec<(String, Vec<usize>)> = held
        .into_iter()
        .map(|(subject, at)| {
            let mut at: Vec<usize> = at.into_iter().collect();
            at.sort_by(|one, other| quieter(&holders[*one], &holders[*other]));
            (subject, at)
        })
        .collect();
    subjects.sort_by_key(|(subject, at)| (at.len(), subject.clone()));
    let mut hubbing = vec![0_usize; holders.len()];
    subjects
        .into_iter()
        .map(|(subject, at)| {
            let hubs = hubs_of(&at, &mut hubbing);
            let listed: Vec<serde_json::Value> = hubs
                .iter()
                .enumerate()
                .map(|(nth, hub)| {
                    let fits: Vec<&str> = at
                        .iter()
                        .skip(nth)
                        .step_by(hubs.len())
                        .take(MOST_FITS)
                        .map(|fit| holders[*fit].address.as_str())
                        .collect();
                    json!({"hub": holders[*hub].address, "fits": fits})
                })
                .collect();
            // The first hub stands alone too, for a reader of one hub.
            let fields = json!({
                "subject": subject,
                "hub": listed[0]["hub"],
                "fits": listed[0]["fits"],
                "hubs": listed,
            });
            let key = daemon::key(KIND, &subject);
            (key, daemon::record(KIND, &subject, 0, fields))
        })
        .collect()
}

/// The hubs of a subject held by `at`, least heard first: one for every
/// [`HOLDERS_A_HUB`] of them, of those hubbing nothing yet, and where
/// too few are, the rest of them hubbing fewest, so every subject has as
/// many as its holders make.
fn hubs_of(at: &[usize], hubbing: &mut [usize]) -> Vec<usize> {
    let wanted = at.len().div_ceil(HOLDERS_A_HUB);
    let mut hubs: Vec<usize> = at
        .iter()
        .copied()
        .filter(|one| hubbing[*one] == 0)
        .take(wanted)
        .collect();
    if hubs.len() < wanted {
        let mut busy: Vec<usize> = at.iter().copied().filter(|one| hubbing[*one] > 0).collect();
        // Stable, so the least heard of those hubbing alike comes first.
        busy.sort_by_key(|one| hubbing[*one]);
        hubs.extend(busy.into_iter().take(wanted - hubs.len()));
    }
    for hub in &hubs {
        hubbing[*hub] += 1;
    }
    hubs
}

/// Which of two cells was heard less lately, the address settling a tie.
fn quieter(one: &Holder, other: &Holder) -> std::cmp::Ordering {
    one.heard
        .total_cmp(&other.heard)
        .then_with(|| one.address.cmp(&other.address))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(address: &str, subjects: &[&str], heard: f64) -> Holder {
        Holder {
            address: address.to_owned(),
            subjects: subjects.iter().map(|&one| one.to_owned()).collect(),
            heard,
        }
    }

    fn hubs(records: &BTreeMap<String, serde_json::Value>, subject: &str) -> Vec<String> {
        records[&daemon::key(KIND, subject)]["hubs"]
            .as_array()
            .expect("a subject lists its hubs")
            .iter()
            .map(|hub| hub["hub"].as_str().expect("a hub is an address").to_owned())
            .collect()
    }

    #[test]
    fn a_subjects_hub_is_its_least_heard_holder_and_names_its_fits() {
        let made = records(&[
            holder("KESTRL", &["mycelium:hubs"], 3.0),
            holder("OSPREY", &["Mycelium:Hubs"], 0.5),
            holder("TERNSS", &["mycelium:hubs"], 1.0),
        ]);
        let record = &made["hub:mycelium:hubs"];
        assert_eq!(
            record["hub"], "OSPREY",
            "the quietest holder hubs it: {record}"
        );
        assert_eq!(
            record["fits"],
            json!(["OSPREY", "TERNSS", "KESTRL"]),
            "its fits are its holders, quietest first, the case it was said in aside"
        );
        assert_eq!(
            record["hubs"][0],
            json!({"hub": "OSPREY", "fits": record["fits"]})
        );
        assert_eq!(record["kind"], KIND);
    }

    #[test]
    fn cells_saying_a_subject_in_any_case_share_its_hub_as_the_cap_folds_it() {
        // Each cell's subjects pass the cap alone, as the node's do, so
        // only the hub's own fold can join what two cells say.
        let cell = |address: &str, said: serde_json::Value, heard: f64| Holder {
            address: address.to_owned(),
            subjects: networks::subjects(&said),
            heard,
        };
        let made = records(&[
            cell("KESTRL", json!(["école", "Straße"]), 0.0),
            cell("OSPREY", json!(["ÉCOLE", "STRASSE"]), 1.0),
        ]);
        assert_eq!(
            made.keys().cloned().collect::<Vec<_>>(),
            ["hub:strasse", "hub:straße", "hub:école"],
            "an accented subject in two cases is one hub, and two the cap keeps apart are two"
        );
        assert_eq!(
            made["hub:école"]["fits"],
            json!(["KESTRL", "OSPREY"]),
            "both cells fit the one they said in different cases"
        );
    }

    #[test]
    fn a_cell_hubs_the_rarest_of_its_subjects_and_a_lone_holder_hubs_each() {
        let made = records(&[
            holder("KESTRL", &["common", "rare"], 0.0),
            holder("OSPREY", &["common"], 1.0),
            holder("SKIFFS", &["lone", "lonely"], 0.0),
        ]);
        assert_eq!(
            hubs(&made, "rare"),
            ["KESTRL"],
            "the rarest is hubbed first"
        );
        assert_eq!(
            hubs(&made, "common"),
            ["OSPREY"],
            "a cell hubs one subject while another holder is free"
        );
        assert_eq!(hubs(&made, "lone"), ["SKIFFS"]);
        assert_eq!(
            hubs(&made, "lonely"),
            ["SKIFFS"],
            "a subject whose every holder hubs another still has a hub"
        );
    }

    #[test]
    fn a_subject_has_a_hub_for_every_ten_holders_each_with_at_most_five_fits() {
        let many: Vec<Holder> = (0..21)
            .map(|n| holder(&format!("K{n:05}"), &["busy"], f64::from(n)))
            .collect();
        let made = records(&many);
        assert_eq!(hubs(&made, "busy"), ["K00000", "K00001", "K00002"]);
        let record = &made["hub:busy"];
        for hub in record["hubs"].as_array().expect("its hubs") {
            let fits = hub["fits"].as_array().expect("its fits");
            assert_eq!(fits.len(), MOST_FITS, "{hub}");
        }
        assert_eq!(
            record["hubs"][1]["fits"],
            json!(["K00001", "K00004", "K00007", "K00010", "K00013"]),
            "the holders are shared out among the hubs, so no two name the same fit"
        );
    }

    #[test]
    fn a_subject_whose_holders_each_hub_a_rarer_one_keeps_a_hub_for_every_ten() {
        // Each of 21 holders also says a subject of its own, which it hubs
        // first, so none is free when the shared one's turn comes.
        let many: Vec<Holder> = (0..21)
            .map(|n| {
                let own = format!("own {n}");
                holder(&format!("K{n:05}"), &["busy", &own], f64::from(n))
            })
            .collect();
        let made = records(&many);
        assert_eq!(
            hubs(&made, "busy"),
            ["K00000", "K00001", "K00002"],
            "the least heard of those hubbing fewest make up its three"
        );
        let fits: usize = made["hub:busy"]["hubs"]
            .as_array()
            .expect("its hubs")
            .iter()
            .map(|hub| hub["fits"].as_array().expect("its fits").len())
            .sum();
        assert_eq!(fits, 15, "three hubs name five fits each");
        // Two free of the three it wants: the third is one hubbing already.
        let mut some = many.clone();
        some[20].subjects = vec!["busy".to_owned()];
        some[19].subjects = vec!["busy".to_owned()];
        assert_eq!(
            hubs(&records(&some), "busy"),
            ["K00019", "K00020", "K00000"],
            "the free come first, then the least heard of those hubbing already"
        );
    }

    #[test]
    fn a_holder_hubbing_more_is_passed_over_for_one_hubbing_fewer() {
        // Each of 21 holders hubs a subject of its own first, and K00001
        // hubs a second, so when the shared one's turn comes it hubs two
        // while every other holder hubs one.
        let many: Vec<Holder> = (0..21)
            .map(|n| {
                let own = format!("own {n}");
                let mut says = vec!["busy", own.as_str()];
                if n == 1 {
                    says.push("own more");
                }
                holder(&format!("K{n:05}"), &says, f64::from(n))
            })
            .collect();
        assert_eq!(
            hubs(&records(&many), "busy"),
            ["K00000", "K00002", "K00003"],
            "K00001 is heard less than K00002, but hubs two to their one"
        );
    }

    #[test]
    fn a_cell_is_heard_by_its_own_talk_an_hours_outweighing_a_days() {
        // As `tie::records` makes a cell's own record.
        let cell = json!({"key": "tie:KESTRL", "kind": "tie", "hour": 2, "today": 48});
        assert_eq!(heard(&cell), Some(("KESTRL".to_owned(), 4.0)));
        let quiet = json!({"key": "tie:OSPREY", "kind": "tie", "hour": 0, "today": 47});
        assert!(heard(&quiet).expect("read").1 < heard(&cell).expect("read").1);
        let tie = json!({"key": "tie:KESTRL~OSPREY", "kind": "tie", "hour": [1, 0]});
        assert_eq!(heard(&tie), None, "a tie between two is no cell's own talk");
    }

    #[test]
    fn a_realm_declaring_nothing_has_no_hubs() {
        assert!(records(&[holder("KESTRL", &[], 0.0)]).is_empty());
    }
}
