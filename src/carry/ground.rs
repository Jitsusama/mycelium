//! Whether a carry an orchestrator orders lands on ground at least as
//! firm as the session stands on here: a machine of this realm, in reach,
//! that runs sessions in the fence this one runs in, with no door of that
//! fence wider than here's. Both machines say their fences as their built
//! configurations declare them, and the session says its own in its
//! presence; where either can't be told, the carry is refused, never
//! guessed.

use std::collections::BTreeSet;

use crate::declared::{Declared, Doors, Kind};
use crate::facts::{Machine, Readiness};

/// Whether a session running in the fence `kind`, as its presence says,
/// may be carried from here, as this machine declares itself, to `to`,
/// as `there` last said itself where the realm knows it. Refused, saying
/// why, where the ground there is looser or can't be told.
///
/// # Errors
///
/// When `to` isn't one of this realm's machines in reach, or it or this
/// machine declares no fence for `kind`, or its doors for it are wider.
pub fn firm(
    kind: Option<Kind>,
    here: Option<&Declared>,
    to: &str,
    there: Option<&Machine>,
) -> Result<(), String> {
    let there = there.ok_or_else(|| {
        format!("{to} isn't one of this realm's machines, so nothing is carried there")
    })?;
    if there.state == Readiness::Unreachable {
        return Err(format!(
            "{to} is out of reach, so nothing is carried there now"
        ));
    }
    let kind = kind.ok_or(
        "the session doesn't say which fence it runs in, so where it lands can't be judged",
    )?;
    let fences = there
        .declared
        .as_ref()
        .and_then(|declared| declared.fence.as_ref())
        .ok_or_else(|| {
            format!("{to} declares no fence, so the session can't be told to land as {kind} there")
        })?;
    let theirs = fences.get(&kind).ok_or_else(|| {
        format!("{to} runs no {kind} session, so the session would land looser there")
    })?;
    let ours = here
        .and_then(|declared| declared.fence.as_ref())
        .and_then(|fences| fences.get(&kind))
        .ok_or_else(|| {
            format!("this machine declares no {kind} session, so {to}'s can't be judged against it")
        })?;
    let opened = wider(theirs, ours);
    if opened.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{to}'s {kind} session opens {}, which here's doesn't, so the session would land \
             looser there",
            opened.join(" and ")
        ))
    }
}

/// What `there` opens that `here` doesn't, each kind of door named with
/// its names, such as `builders big`.
fn wider(there: &Doors, here: &Doors) -> Vec<String> {
    let beyond = |theirs: &BTreeSet<String>, ours: &BTreeSet<String>| {
        theirs
            .difference(ours)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    [
        ("builders", beyond(&there.builders, &here.builders)),
        ("forges", beyond(&there.forges, &here.forges)),
        ("secrets", beyond(&there.secrets, &here.secrets)),
    ]
    .into_iter()
    .filter(|(_, names)| !names.is_empty())
    .map(|(door, names)| format!("{door} {names}"))
    .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::facts;

    fn doors(builders: &[&str], forges: &[&str], secrets: &[&str]) -> Doors {
        let names = |all: &[&str]| all.iter().map(|&one| one.to_owned()).collect();
        Doors {
            builders: names(builders),
            forges: names(forges),
            secrets: names(secrets),
        }
    }

    fn declaring(fence: Option<BTreeMap<Kind, Doors>>) -> Declared {
        Declared {
            reach: Vec::new(),
            probes: BTreeMap::new(),
            outlives: Vec::new(),
            costs_money: false,
            taints: Vec::new(),
            cores: None,
            agents: BTreeMap::new(),
            leaves_at: None,
            within: None,
            hosts: Vec::new(),
            fence,
        }
    }

    fn strict(doors: Doors) -> BTreeMap<Kind, Doors> {
        BTreeMap::from([(Kind::Strict, doors)])
    }

    fn tern(fence: Option<BTreeMap<Kind, Doors>>, state: Readiness) -> Machine {
        let nowhere = facts::Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: "/nowhere".into(),
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        Machine {
            state,
            declared: Some(declaring(fence)),
            ..facts::measure("tern", &nowhere)
        }
    }

    fn here() -> Declared {
        declaring(Some(strict(doors(&["remote"], &["github"], &["registry"]))))
    }

    #[test]
    fn a_session_lands_where_its_fence_is_as_shut() {
        let there = tern(Some(strict(doors(&["remote"], &[], &[]))), Readiness::Ready);
        assert_eq!(
            firm(Some(Kind::Strict), Some(&here()), "tern", Some(&there)),
            Ok(()),
            "the same fence with fewer doors is firm ground"
        );
    }

    #[test]
    fn a_machine_not_of_this_realm_or_out_of_reach_takes_nothing() {
        assert_eq!(
            firm(Some(Kind::Strict), Some(&here()), "elsewhere", None),
            Err(
                "elsewhere isn't one of this realm's machines, so nothing is carried there"
                    .to_owned()
            ),
            "a machine the realm doesn't know, as one of another realm"
        );
        let away = tern(Some(strict(doors(&[], &[], &[]))), Readiness::Unreachable);
        assert_eq!(
            firm(Some(Kind::Strict), Some(&here()), "tern", Some(&away)),
            Err("tern is out of reach, so nothing is carried there now".to_owned()),
            "a machine no link reaches now"
        );
    }

    #[test]
    fn a_fence_that_cant_be_told_refuses_the_carry() {
        let there = tern(Some(strict(doors(&[], &[], &[]))), Readiness::Ready);
        assert_eq!(
            firm(None, Some(&here()), "tern", Some(&there)),
            Err(
                "the session doesn't say which fence it runs in, so where it lands can't be \
                 judged"
                    .to_owned()
            ),
            "a session whose presence says no fence"
        );
        let bare = tern(None, Readiness::Ready);
        assert_eq!(
            firm(Some(Kind::Strict), Some(&here()), "tern", Some(&bare)),
            Err(
                "tern declares no fence, so the session can't be told to land as \
                 strictly fenced there"
                    .to_owned()
            ),
            "a machine declaring no fence"
        );
        assert_eq!(
            firm(
                Some(Kind::Strict),
                Some(&declaring(None)),
                "tern",
                Some(&there)
            ),
            Err(
                "this machine declares no strictly fenced session, so tern's can't be \
                 judged against it"
                    .to_owned()
            ),
            "this machine declaring no fence"
        );
    }

    #[test]
    fn a_fenced_session_never_lands_looser() {
        let fenced_only = tern(
            Some(BTreeMap::from([(Kind::Fence, doors(&[], &[], &[]))])),
            Readiness::Ready,
        );
        assert_eq!(
            firm(
                Some(Kind::Strict),
                Some(&here()),
                "tern",
                Some(&fenced_only)
            ),
            Err(
                "tern runs no strictly fenced session, so the session would land \
                 looser there"
                    .to_owned()
            ),
            "a machine without the session's fence"
        );
        let wide = tern(
            Some(strict(doors(
                &["remote", "big"],
                &["github"],
                &["registry", "cloud"],
            ))),
            Readiness::Ready,
        );
        assert_eq!(
            firm(Some(Kind::Strict), Some(&here()), "tern", Some(&wide)),
            Err(
                "tern's strictly fenced session opens builders big and secrets cloud, \
                 which here's doesn't, so the session would land looser there"
                    .to_owned()
            ),
            "a machine whose doors are wider names each"
        );
    }
}
