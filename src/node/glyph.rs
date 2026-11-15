//! The shape each team is known by, chosen once here so the map, the
//! terminal and the launcher draw an orchestration alike.
//!
//! There are seven shapes. A team's first choice comes from its network's
//! name, by the hash the map page has always used, so a team keeps its
//! shape as others come and go. The oldest team chooses first, so a new
//! one never takes an older one's, and one whose first choice is taken
//! steps on to the next free shape. Past seven teams two must share, so a
//! team takes the shape held by the fewest teams it is seen beside (those
//! within the same team as it, and the team it is within or holds), then
//! by the fewest teams at all.

use std::collections::BTreeMap;

/// Every shape, in the order the map page steps through them.
pub const GLYPHS: [&str; 7] = [
    "square",
    "triangle",
    "plus",
    "hexagon",
    "circle",
    "down-triangle",
    "star",
];

/// One team as its shape is chosen: its id, what sorts it among the
/// others by when it began, and the team it is within.
#[derive(Clone, Copy, Debug)]
pub struct Choosing<'a> {
    /// The team's id, its orchestrator's address.
    pub id: &'a str,
    /// What sorts it by when it began, such as its oldest session's id.
    pub began: &'a str,
    /// The team it is within, by id.
    pub within: Option<&'a str>,
}

impl Choosing<'_> {
    /// Whether it and `other` are drawn beside each other: within the same
    /// team, or one within the other.
    fn beside(&self, other: &Self) -> bool {
        self.within == other.within
            || self.within == Some(other.id)
            || other.within == Some(self.id)
    }
}

/// The map page's hash of `name`: each UTF-16 unit added to 31 times what
/// came before, wrapping at 32 bits.
fn hashed(name: &str) -> u32 {
    name.encode_utf16().fold(0_u32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(u32::from(unit))
    })
}

/// The shape `id`'s team would take with none taken: its network's name
/// hashed as the map page hashes it.
fn first_choice(id: &str) -> usize {
    let name = format!("orchestration.{}", id.to_lowercase());
    usize::try_from(hashed(&name)).unwrap_or_default() % GLYPHS.len()
}

/// Each of `teams`' shapes, by its id.
#[must_use]
pub fn glyphs<'a>(teams: &[Choosing<'a>]) -> BTreeMap<&'a str, &'static str> {
    let mut oldest_first: Vec<&Choosing> = teams.iter().collect();
    oldest_first.sort_by_key(|team| (team.began, team.id));
    let mut chosen: Vec<(&Choosing, usize)> = Vec::with_capacity(teams.len());
    for team in oldest_first {
        let first = first_choice(team.id);
        let crowding = |shape: usize| {
            let holders = chosen.iter().filter(|(_, held)| *held == shape);
            let beside = holders.clone().filter(|(other, _)| team.beside(other));
            (beside.count(), holders.count())
        };
        // The first of the least crowded, stepping on from its first
        // choice, so with any shape free it takes the first free one.
        let shape = (0..GLYPHS.len())
            .map(|step| (first + step) % GLYPHS.len())
            .min_by_key(|shape| crowding(*shape))
            .unwrap_or(first);
        chosen.push((team, shape));
    }
    chosen
        .into_iter()
        .map(|(team, shape)| (team.id, GLYPHS[shape]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Choosing, glyphs};

    /// A team `id`, begun at `began`, within `within`.
    fn team<'a>(id: &'a str, began: &'a str, within: Option<&'a str>) -> Choosing<'a> {
        Choosing { id, began, within }
    }

    #[test]
    fn a_team_takes_the_shape_the_map_page_gave_its_name() {
        let chosen = glyphs(&[team("HARBOR", "1", None), team("REEF01", "2", None)]);
        assert_eq!(
            (chosen["HARBOR"], chosen["REEF01"]),
            ("square", "triangle"),
            "each its name's own, as the map page hashes orchestration.harbor and \
             orchestration.reef01"
        );
    }

    #[test]
    fn the_older_of_two_wanting_one_shape_keeps_it_and_the_newer_steps_on() {
        let chosen = glyphs(&[team("HARBOR", "1", None), team("KELP01", "2", None)]);
        assert_eq!(
            (chosen["HARBOR"], chosen["KELP01"]),
            ("square", "triangle"),
            "both hash to square, and Harbour began first"
        );
        let chosen = glyphs(&[team("HARBOR", "2", None), team("KELP01", "1", None)]);
        assert_eq!(
            (chosen["HARBOR"], chosen["KELP01"]),
            ("triangle", "square"),
            "which began first decides, not the names"
        );
    }

    #[test]
    fn past_seven_a_team_shares_a_shape_with_one_it_is_never_seen_beside() {
        let mut teams = [
            ("HARBOR", "square"),
            ("REEF01", "triangle"),
            ("CRAB01", "plus"),
            ("PIER01", "hexagon"),
            ("DOCK01", "circle"),
            ("TIDE01", "down-triangle"),
            ("SKIFF1", "star"),
        ]
        .map(|(id, _)| team(id, "1", None))
        .to_vec();
        teams.push(team("KELP01", "2", Some("HARBOR")));
        teams.push(team("GULL01", "3", Some("HARBOR")));
        let chosen = glyphs(&teams);
        assert_eq!(
            [
                "HARBOR", "REEF01", "CRAB01", "PIER01", "DOCK01", "TIDE01", "SKIFF1"
            ]
            .map(|id| chosen[id]),
            [
                "square",
                "triangle",
                "plus",
                "hexagon",
                "circle",
                "down-triangle",
                "star"
            ],
            "the first seven each take their own"
        );
        assert_eq!(
            (chosen["KELP01"], chosen["GULL01"]),
            ("triangle", "plus"),
            "Kelp steps past Harbour's square, being within it, to Reef's triangle; Gull \
             wants that, but Kelp beside it holds it, so it takes Crab's plus"
        );
    }
}
