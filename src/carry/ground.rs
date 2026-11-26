//! Whether a carry an orchestrator orders lands on ground at least as
//! firm as the session stands on here: a machine of this realm, in reach,
//! that runs sessions in the fence this one runs in, with no door of that
//! fence wider than here's. Both machines say their fences as their built
//! configurations declare them, and the session says its own in its
//! presence; where either can't be told, the carry is refused, never
//! guessed.

use std::collections::BTreeSet;

use crate::declared::{Accepted, Declared, Doors, Effect, Kind, Taint, Unrunnable};
use crate::facts::{HeldFor, Machine, Readiness};
use crate::placement::{Because, LIFTS_FENCE, LOOSER_GROUND, Miss, RUNS_NOTHING};

/// The fence the session `address` is judged to run in: the one its
/// presence `claimed`, refused where its process `shown` it stricter,
/// since a session saying a looser fence than it runs in would be carried
/// as one, onto looser ground. A claim stricter than what is shown stands,
/// as the stricter of the two, since the strict fence leaves nothing
/// lasting to show. A session claiming none is left for [`firm`] to refuse.
///
/// # Errors
///
/// When `claimed` is looser than `shown`.
pub fn as_shown(claimed: Option<Kind>, shown: Kind, address: &str) -> Result<Option<Kind>, String> {
    match claimed {
        Some(kind) if kind < shown => Err(format!(
            "{address} says it runs {kind}, but its process shows it {shown}, so where it may \
             land can't be told"
        )),
        _ => Ok(claimed),
    }
}

/// The fence a carry would judge a session by, `claimed` by its presence
/// and `shown` by its process; none where the carry would refuse it for
/// its fence, a claim missing or looser than what shows.
#[must_use]
pub fn judged_as_carried(claimed: Option<Kind>, shown: Kind) -> Option<Kind> {
    as_shown(claimed, shown, "").ok().flatten()
}

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
    worker_takes(kind, to, there)?;
    stands(kind, here, to, there.declared.as_ref())
}

/// What a machine a carry would go to is: one of the person's own, or a
/// worker, which its link marks it or its own word says it is, that
/// declares it takes carries or takes none. One of the person's own in
/// the cloud is paid for, and a room its node names for agents is lent,
/// but a carry takes either as it takes any of theirs ([`Lent::at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lent {
    /// One of the person's own machines, in the cloud or not.
    Not,
    /// A worker that declares it takes carried sessions.
    TakesCarries,
    /// A worker that doesn't.
    TakesNone,
}

impl Lent {
    /// What `machine` is, as the realm last knew it.
    #[must_use]
    pub fn of(machine: &Machine) -> Self {
        Self::at(machine, jiff::Timestamp::now())
    }

    /// What `machine` is at `now`, as the realm last knew it.
    #[must_use]
    pub fn at(machine: &Machine, now: jiff::Timestamp) -> Self {
        let declared = machine.declared.as_ref();
        if !(machine.worker || declared.is_some_and(Declared::is_worker))
            || declared
                .is_some_and(|said| in_the_cloud(machine, said, now) || a_room(machine, said))
        {
            Self::Not
        } else if declared.is_some_and(|said| said.takes_carries) {
            Self::TakesCarries
        } else {
            Self::TakesNone
        }
    }
}

/// How long a machine in the cloud must yet stay for a carry to take it
/// as one of the person's own, since a carried session lasts.
const STAYS_FOR: jiff::SignedDuration = jiff::SignedDuration::from_hours(24 * 30);

/// Whether `machine`, as it `said` itself, is one of the person's own in
/// the cloud at `now`: paid for, perhaps, but linked as one of theirs
/// rather than lent by a provider's link, reaching no network only a
/// lent machine reaches, and staying a month at least. The person ruled
/// on 26 November that such a machine is a full place for sessions, as
/// the Mac is, with no word of its own needed; it is held to the same
/// realm, reach and fences as any of theirs.
fn in_the_cloud(machine: &Machine, said: &Declared, now: jiff::Timestamp) -> bool {
    !machine.worker
        && !reaches_aquifer(said)
        && said
            .leaves_at
            .is_none_or(|leaves| now.checked_add(STAYS_FOR).is_ok_and(|month| leaves > month))
}

/// Whether `machine`, as it `said` itself, is a room its node names for
/// agents to share ([`crate::made::Book::agent_rooms`]) and reaches no
/// network only a lent machine reaches. The person ruled on 26 November
/// that carries go to the cloud machines made for sessions with no word
/// of their own, so the node's naming is enough, and the machine's own
/// word is never asked, since the node sets the mark both ways. It is
/// held to the same realm, reach and fences as any of the person's, and
/// a carry ordered there goes though the room leaves sooner than a month,
/// as its maker chose; an ease or a placement still weighs its leaving.
fn a_room(machine: &Machine, said: &Declared) -> bool {
    machine.held_for == HeldFor::Agents && !reaches_aquifer(said)
}

/// Whether a machine reaches Aquifer, whose cells are lent for its own
/// work alone and keep the worker's rules, since Aquifer's security stands.
fn reaches_aquifer(said: &Declared) -> bool {
    said.reach.iter().any(|network| network == "aquifer")
}

/// Why nothing is carried to `to`, a worker that takes no carries.
#[must_use]
pub fn takes_none(to: &str) -> String {
    format!("{to} is a worker that doesn't declare it takes carries, so nothing is carried there")
}

/// Whether `there`, where it is a worker, takes a carry of a session
/// running in `kind`: only one that declares it takes carries does, and
/// never of a strict session from a node, since a worker's strict fence
/// is its own word, which no node holds it to. One of the person's own
/// machines takes any, as far as this says.
///
/// # Errors
///
/// When `there` is a worker that doesn't declare it takes carries, or
/// the session runs strictly.
pub fn worker_takes(kind: Option<Kind>, to: &str, there: &Machine) -> Result<(), String> {
    match Lent::of(there) {
        Lent::Not => return Ok(()),
        Lent::TakesNone => return Err(takes_none(to)),
        Lent::TakesCarries => {}
    }
    if kind == Some(Kind::Strict) {
        return Err(format!(
            "{to} is a worker, whose strict fence is its own word, so no node carries a strict \
             session there"
        ));
    }
    Ok(())
}

/// Why a machine that would declare `there` is looser ground than this
/// one, as `here` declares it, for a session in the fence `kind`; none
/// where it is as firm. A machine not yet made is judged by what its
/// offer says it would declare, so none is made, and paid for, only to be
/// refused once it links.
#[must_use]
pub fn looser_offered(
    kind: Kind,
    here: Option<&Declared>,
    to: &str,
    there: Option<&Declared>,
) -> Option<String> {
    stands(Some(kind), here, to, there).err()
}

/// Each of `machines` work asked by an agent running in the fence `kind`
/// may not be placed on from `this`, as this machine declares itself in
/// `here`, refused saying why: every other machine whose ground isn't as
/// firm, as [`firm`] judges it, so placement never offers a machine that
/// would refuse to run the work once it got there. This one is never
/// among them, since the asker already stands on its ground, and a machine
/// out of reach is judged by its ground alone, so placement can still say
/// it would do once it is back.
#[must_use]
pub fn looser(kind: Kind, here: Option<&Declared>, this: &str, machines: &[Machine]) -> Vec<Miss> {
    machines
        .iter()
        .filter(|machine| machine.name != this)
        .filter_map(|machine| {
            let why = stands(Some(kind), here, &machine.name, machine.declared.as_ref()).err()?;
            Some(Miss {
                machine: machine.name.clone(),
                because: vec![Because::Refuses(Taint {
                    key: LOOSER_GROUND.to_owned(),
                    effect: Effect::Never,
                    why,
                })],
            })
        })
        .collect()
}

/// Each of `machines` an agent's run asked in the fence `kind` may not be
/// placed on, beside those [`looser`] refuses, refused saying why: `this`
/// machine where its own launcher refuses the run (`here`, as
/// [`crate::declared::launcher`] judges it, with the store at hand), and
/// every other machine whose fence for `kind`, as it last said it, runs
/// no command by its words alone ([`crate::declared::unrunnable`]), since
/// that machine's launcher refuses the run there. A node says a fence's
/// `runs` only where its own launcher accepts them, so one whose launcher
/// refuses for what its words don't show (a declaration not sealed, a
/// program missing or outside the store) says none. A machine whose node
/// is too old to say its fence's `runs` says none too, so it is refused
/// until it is upgraded. A machine that runs no `kind` session at all is
/// [`looser`]'s to refuse; one whose doors are wider and whose `runs` runs
/// nothing is refused by both, each saying its own why.
#[must_use]
pub fn runs_nothing(
    kind: Kind,
    this: &str,
    here: Result<(), String>,
    machines: &[Machine],
) -> Vec<Miss> {
    let refused = |machine: &str, why: String| Miss {
        machine: machine.to_owned(),
        because: vec![Because::Refuses(Taint {
            key: RUNS_NOTHING.to_owned(),
            effect: Effect::Never,
            why,
        })],
    };
    let here = here.err().map(|why| refused(this, why));
    let there = machines
        .iter()
        .filter(|machine| machine.name != this)
        .filter_map(|machine| {
            let why = runs_nothing_there(kind, &machine.name, machine.declared.as_ref())?;
            Some(refused(&machine.name, why))
        });
    here.into_iter().chain(there).collect()
}

/// Each of `machines` an agent's held start in the fence `kind` may not
/// be placed on, this one among them, refused saying why: every Mac whose
/// node doesn't say it starts one through the launcher its fence declares
/// ([`Machine::fences_held`]), whatever it declares. Such a node starts it
/// through the login shell alone, and pi lifts its own fence on a Mac,
/// which has no Landlock, so the session would run there outside any
/// fence. A Mac whose node says it does is judged as any machine is: by
/// [`looser`] and [`runs_nothing`], the fence its launcher declares.
#[must_use]
pub fn lifts_fence(kind: Kind, machines: &[Machine]) -> Vec<Miss> {
    machines
        .iter()
        .filter(|machine| machine.system.as_deref() == Some(MAC) && !machine.fences_held)
        .map(|machine| Miss {
            machine: machine.name.clone(),
            because: vec![Because::Refuses(Taint {
                key: LIFTS_FENCE.to_owned(),
                effect: Effect::Never,
                why: format!(
                    "{} is a Mac, where a {kind} session started held would run outside any \
                     fence",
                    machine.name
                ),
            })],
        })
        .collect()
}

/// Why a machine made of the offer `to`, a `system` machine, would take
/// no agent's held start in the fence `kind`, if it wouldn't: a Mac, as
/// [`lifts_fence`] judges one of the realm whose node doesn't say it
/// fences one, since an offer can't say which node it would run, so none
/// is made, and paid for, only to be refused once it links.
#[must_use]
pub fn lifts_fence_offered(kind: Kind, to: &str, system: Option<&str>) -> Option<String> {
    (system == Some(MAC)).then(|| {
        format!(
            "{to} would be a Mac, where a {kind} session started held would run outside any fence"
        )
    })
}

/// The system a Mac says it is, as Rust names macOS.
const MAC: &str = "macos";

/// Why a machine made of the offer `to`, declaring itself as `there`
/// says it would, runs no command an agent asks in the fence `kind`, as
/// [`runs_nothing`] judges a machine of the realm by its words alone, so
/// none is made, and paid for, only to be refused once it links. An offer
/// that declares no `kind` fence is [`looser_offered`]'s to refuse. One
/// naming no `runs` is told as its provider's to mend, since no node of
/// it runs yet to be too old.
#[must_use]
pub fn runs_nothing_offered(kind: Kind, to: &str, there: Option<&Declared>) -> Option<String> {
    let doors = there
        .and_then(|declared| declared.fence.as_ref())
        .and_then(|fences| fences.get(&kind))?;
    match crate::declared::unrunnable(&doors.runs)? {
        Unrunnable::Nothing => Some(format!(
            "{to}'s provider promises no way to run a command as its {kind} sessions run, so \
             its offer must name the fence's runs"
        )),
        _ => runs_nothing_there(kind, to, there),
    }
}

/// Why the start that brings a session carried to `to`, declaring itself
/// as `declared` says, back would be refused there, if it would. That
/// start is an agent's, the orchestrator's or the node's own, judged in
/// whichever of fence and strict its asker runs, and each kind declares
/// its own `runs`, so a carry is refused when either declared runs
/// nothing. It is held too, so a carry to a Mac, a `system` machine as
/// [`lifts_fence`] judges one, is refused whatever it declares where its
/// node doesn't say it `fences_held` starts, unless the session runs in
/// `kind` [`Kind::None`]: one already unfenced here runs no looser
/// unfenced there, and [`firm`] still holds its doors to this machine's.
/// A session whose fence can't be told is refused every Mac, since its
/// node would start it within no launcher.
#[must_use]
pub fn brought_back(
    to: &str,
    declared: Option<&Declared>,
    system: Option<&str>,
    fences_held: bool,
    kind: Option<Kind>,
) -> Option<String> {
    if system == Some(MAC) && kind.is_none() {
        return Some(format!(
            "{to} is a Mac, where a session whose fence can't be told would be brought back \
             held outside any fence"
        ));
    }
    if system == Some(MAC) && !fences_held && kind != Some(Kind::None) {
        return Some(format!(
            "{to} is a Mac, where a fenced session brought back held would run outside any \
             fence"
        ));
    }
    [Kind::Fence, Kind::Strict]
        .into_iter()
        .find_map(|kind| runs_nothing_there(kind, to, declared))
}

/// Why a session in the fence `kind` may not be carried by a node from
/// `here`, a machine of the system `ours`, to `to`, one of `theirs`, if it
/// may not. A strict session goes only between machines of one system,
/// since each system's strict fence reads and writes its own way, a Mac's
/// more of the home folder than Linux's, so one carried across would land
/// looser though both say strict. Where either system can't be told, it is
/// taken as another. A node makes every carry an agent asks for, easing's,
/// an orchestrator's and a session's of itself, so only the person, whose
/// carry no node makes, takes a strict session across.
#[must_use]
pub fn alike(
    kind: Option<Kind>,
    (here, ours): (&str, Option<&str>),
    (to, theirs): (&str, Option<&str>),
) -> Option<String> {
    if kind != Some(Kind::Strict) {
        return None;
    }
    match (ours, theirs) {
        (None, _) => Some(format!(
            "{here} can't say its own system, so only the person carries a strict session from it"
        )),
        (Some(_), None) => Some(format!(
            "{to} doesn't say its system, so only the person carries a strict session there"
        )),
        (Some(ours), Some(theirs)) if ours != theirs => Some(format!(
            "{to} runs {theirs}, whose strict fence reads and writes other than {ours}'s, so \
             only the person carries a strict session there"
        )),
        (Some(_), Some(_)) => None,
    }
}

/// Why `name`, declaring itself as `declared` says, runs no command in its
/// `kind` fence by that fence's words alone, if it doesn't.
fn runs_nothing_there(kind: Kind, name: &str, declared: Option<&Declared>) -> Option<String> {
    let doors = declared
        .and_then(|declared| declared.fence.as_ref())
        .and_then(|fences| fences.get(&kind))?;
    Some(match crate::declared::unrunnable(&doors.runs)? {
        Unrunnable::Nothing => format!(
            "{name} declares no way to run a command as its {kind} sessions run that its \
             launcher accepts, or its node is too old to say one"
        ),
        Unrunnable::SparesATree(spare) => format!(
            "{name}'s {kind} sessions' launcher says {}, which spares a tree shared to the \
             asker, so it runs no agent's command",
            said_word(&spare)
        ),
        Unrunnable::Unended => format!(
            "{name}'s {kind} sessions' launcher doesn't end with --, so it runs no agent's \
             command"
        ),
    })
}

/// The longest a word another machine says is quoted in a refusal.
const WORD_SAID: usize = 64;

/// `word`, as another machine said it, as a refusal quotes it to an agent:
/// its control characters escaped and cut to [`WORD_SAID`] characters, so
/// what that machine chose to say can't pass as the node's own words or
/// break the log's one line.
fn said_word(word: &str) -> String {
    let escaped = crate::declared::controls_escaped(word);
    if escaped.chars().count() <= WORD_SAID {
        return escaped;
    }
    let cut: String = escaped.chars().take(WORD_SAID).collect();
    format!("{cut}...")
}

/// Whether `there`, what `to` declares as it last said itself, runs
/// sessions in the fence `kind` with no door wider than this machine's,
/// as `here` declares it, but those here's `accepts` allows for `to` by
/// name.
fn stands(
    kind: Option<Kind>,
    here: Option<&Declared>,
    to: &str,
    there: Option<&Declared>,
) -> Result<(), String> {
    let kind = kind.ok_or(
        "the session doesn't say which fence it runs in, so where it lands can't be judged",
    )?;
    let fences = there
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
    let opened = wider(theirs, ours, ours.accepts.get(to));
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

/// What `there` opens that `here` doesn't and `accepted` doesn't allow,
/// each kind of door named with its names, such as `builders big`.
fn wider(there: &Doors, here: &Doors, accepted: Option<&Accepted>) -> Vec<String> {
    let none = Accepted::default();
    let accepted = accepted.unwrap_or(&none);
    let beyond =
        |theirs: &BTreeSet<String>, ours: &BTreeSet<String>, allowed: &BTreeSet<String>| {
            theirs
                .iter()
                .filter(|name| !ours.contains(*name) && !allowed.contains(*name))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
    [
        (
            "builders",
            beyond(&there.builders, &here.builders, &accepted.builders),
        ),
        (
            "forges",
            beyond(&there.forges, &here.forges, &accepted.forges),
        ),
        (
            "secrets",
            beyond(&there.secrets, &here.secrets, &accepted.secrets),
        ),
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
    use crate::{facts, placement};

    #[test]
    fn a_session_claiming_a_looser_fence_than_its_process_shows_is_refused() {
        assert_eq!(
            as_shown(Some(Kind::None), Kind::Fence, "HARBOR"),
            Err(
                "HARBOR says it runs unfenced, but its process shows it fenced, so where it \
                 may land can't be told"
                    .to_owned()
            ),
            "never carried as looser than it runs"
        );
        assert_eq!(
            as_shown(Some(Kind::Fence), Kind::Strict, "HARBOR"),
            Err(
                "HARBOR says it runs fenced, but its process shows it strictly fenced, so \
                 where it may land can't be told"
                    .to_owned()
            ),
            "nor fenced where it runs strictly"
        );
        for (claimed, shown) in [
            (Kind::Strict, Kind::None),
            (Kind::Strict, Kind::Strict),
            (Kind::Fence, Kind::Fence),
            (Kind::None, Kind::None),
        ] {
            assert_eq!(
                as_shown(Some(claimed), shown, "HARBOR"),
                Ok(Some(claimed)),
                "the stricter of the two, claimed {claimed} and shown {shown}"
            );
        }
        assert_eq!(
            as_shown(None, Kind::Strict, "HARBOR"),
            Ok(None),
            "no claim is left for the ground to refuse"
        );
    }

    #[test]
    fn a_strict_session_is_carried_by_a_node_only_within_its_system() {
        let strict = Some(Kind::Strict);
        let linux = Some("linux");
        assert_eq!(alike(strict, ("skiff", linux), ("osprey", linux)), None);
        assert_eq!(
            alike(strict, ("skiff", linux), ("tern", Some(MAC))).as_deref(),
            Some(
                "tern runs macos, whose strict fence reads and writes other than linux's, so \
                 only the person carries a strict session there"
            )
        );
        assert_eq!(
            alike(strict, ("skiff", linux), ("tern", None)).as_deref(),
            Some("tern doesn't say its system, so only the person carries a strict session there"),
            "a node too old to say is taken as another system"
        );
        for theirs in [None, linux] {
            assert_eq!(
                alike(strict, ("skiff", None), ("tern", theirs)).as_deref(),
                Some(
                    "skiff can't say its own system, so only the person carries a strict \
                     session from it"
                ),
                "where this machine can't say, none is its own, whatever tern says"
            );
        }
        for kind in [None, Some(Kind::None), Some(Kind::Fence)] {
            assert_eq!(
                alike(kind, ("skiff", linux), ("tern", Some(MAC))),
                None,
                "only a strict session is held to its system, so {kind:?} isn't"
            );
        }
    }

    #[test]
    fn a_look_judges_a_fence_as_a_carry_would() {
        for (claimed, shown, judged) in [
            (None, Kind::Strict, None),
            (Some(Kind::None), Kind::Fence, None),
            (Some(Kind::Fence), Kind::Strict, None),
            (Some(Kind::Strict), Kind::None, Some(Kind::Strict)),
            (Some(Kind::Fence), Kind::Fence, Some(Kind::Fence)),
        ] {
            assert_eq!(
                judged_as_carried(claimed, shown),
                judged,
                "claimed {claimed:?} and shown {shown}"
            );
        }
    }

    fn doors(builders: &[&str], forges: &[&str], secrets: &[&str]) -> Doors {
        let names = |all: &[&str]| all.iter().map(|&one| one.to_owned()).collect();
        Doors {
            builders: names(builders),
            forges: names(forges),
            secrets: names(secrets),
            runs: Vec::new(),
            accepts: BTreeMap::new(),
        }
    }

    fn declaring(fence: Option<BTreeMap<Kind, Doors>>) -> Declared {
        Declared {
            fence,
            ..Declared::default()
        }
    }

    fn strict(doors: Doors) -> BTreeMap<Kind, Doors> {
        BTreeMap::from([(Kind::Strict, doors)])
    }

    fn tern(fence: Option<BTreeMap<Kind, Doors>>, state: Readiness) -> Machine {
        Machine {
            state,
            declared: Some(declaring(fence)),
            ..facts::measure("tern", &facts::Sources::nowhere())
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

    /// A worker, as its link marks it, declaring both fences with no
    /// doors, and whether it `takes` carries.
    fn worker(takes: bool) -> Machine {
        let both = BTreeMap::from([
            (Kind::Fence, doors(&[], &[], &[])),
            (Kind::Strict, doors(&[], &[], &[])),
        ]);
        Machine {
            worker: true,
            declared: Some(Declared {
                takes_carries: takes,
                ..declaring(Some(both))
            }),
            ..tern(None, Readiness::Ready)
        }
    }

    #[test]
    fn a_worker_takes_a_carry_only_where_it_declares_it_does() {
        let here = declaring(Some(BTreeMap::from([
            (Kind::Fence, doors(&[], &[], &[])),
            (Kind::Strict, doors(&[], &[], &[])),
        ])));
        for kind in [Kind::Fence, Kind::Strict] {
            assert_eq!(
                firm(Some(kind), Some(&here), "tern", Some(&worker(false))),
                Err(
                    "tern is a worker that doesn't declare it takes carries, so nothing is \
                     carried there"
                        .to_owned()
                ),
                "a worker saying nothing takes no {kind} session"
            );
        }
        assert_eq!(
            firm(Some(Kind::Fence), Some(&here), "tern", Some(&worker(true))),
            Ok(()),
            "one that declares it takes a fenced session on ground as firm"
        );
        assert_eq!(
            firm(Some(Kind::Strict), Some(&here), "tern", Some(&worker(true))),
            Err(
                "tern is a worker, whose strict fence is its own word, so no node carries a \
                 strict session there"
                    .to_owned()
            ),
            "but never a strict one"
        );
        let lent = Machine {
            worker: false,
            declared: Some(Declared {
                costs_money: true,
                reach: vec!["aquifer".to_owned()],
                ..declaring(None)
            }),
            ..tern(None, Readiness::Ready)
        };
        assert_eq!(
            worker_takes(Some(Kind::Fence), "tern", &lent),
            Err(
                "tern is a worker that doesn't declare it takes carries, so nothing is \
                 carried there"
                    .to_owned()
            ),
            "a machine that reaches what only a lent one does is a worker, whatever its link \
             says"
        );
        assert_eq!(
            worker_takes(Some(Kind::Strict), "tern", &tern(None, Readiness::Ready)),
            Ok(()),
            "one of the person's own machines is no worker"
        );
    }

    /// One of the person's own machines in the cloud: paid for, and saying
    /// it lasts as long as time can, as a provider's machine made to stay
    /// does, with the same fences as `here`.
    fn in_the_cloud(fence: BTreeMap<Kind, Doors>) -> Machine {
        Machine {
            worker: false,
            declared: Some(Declared {
                costs_money: true,
                leaves_at: Some("9999-01-01T00:00:00Z".parse().expect("a time")),
                ..declaring(Some(fence))
            }),
            ..tern(None, Readiness::Ready)
        }
    }

    #[test]
    fn a_machine_of_the_persons_in_the_cloud_takes_any_carry_on_ground_as_firm() {
        let fences = BTreeMap::from([
            (Kind::None, doors(&[], &[], &[])),
            (Kind::Fence, doors(&[], &[], &[])),
            (Kind::Strict, doors(&[], &[], &[])),
        ]);
        let here = declaring(Some(fences.clone()));
        let cloud = in_the_cloud(fences.clone());
        let now: jiff::Timestamp = "2026-11-26T15:00:00Z".parse().expect("a time");
        assert_eq!(
            Lent::at(&cloud, now),
            Lent::Not,
            "paid for, but no provider lent it"
        );
        for kind in [Kind::None, Kind::Fence, Kind::Strict] {
            assert_eq!(
                firm(Some(kind), Some(&here), "tern", Some(&cloud)),
                Ok(()),
                "a {kind:?} session lands there under its own fence, as on any of the \
                 person's machines"
            );
        }
        assert!(
            firm(None, Some(&here), "tern", Some(&cloud)).is_err(),
            "one that doesn't say its fence is refused, as it is everywhere"
        );
        let looser = in_the_cloud(BTreeMap::from([(Kind::Fence, doors(&[], &[], &[]))]));
        assert!(
            firm(Some(Kind::Strict), Some(&here), "tern", Some(&looser)).is_err(),
            "but never onto looser ground"
        );
        let mut gone = cloud.clone();
        gone.state = Readiness::Unreachable;
        assert!(
            firm(Some(Kind::Fence), Some(&here), "tern", Some(&gone)).is_err(),
            "nor out of reach"
        );
        let mut lent = cloud.clone();
        lent.worker = true;
        assert_eq!(
            Lent::at(&lent, now),
            Lent::TakesNone,
            "one a provider's link lent is a worker still"
        );
        let mut leaving = cloud.clone();
        leaving.declared.as_mut().expect("declared").leaves_at =
            Some(now + jiff::SignedDuration::from_hours(24));
        assert_eq!(
            Lent::at(&leaving, now),
            Lent::TakesNone,
            "and one that leaves within the month, since a carried session lasts"
        );
        let mut staying = cloud;
        staying.declared.as_mut().expect("declared").leaves_at = None;
        assert_eq!(
            Lent::at(&staying, now),
            Lent::Not,
            "one that names no time it leaves stays"
        );
    }

    #[test]
    fn a_room_its_node_names_for_agents_takes_any_carry_on_ground_as_firm() {
        let fences = BTreeMap::from([
            (Kind::None, doors(&[], &[], &[])),
            (Kind::Fence, doors(&[], &[], &[])),
            (Kind::Strict, doors(&[], &[], &[])),
        ]);
        let here = declaring(Some(fences.clone()));
        let now: jiff::Timestamp = "2026-11-26T17:00:00Z".parse().expect("a time");
        // Lent by a provider's link, leaving within the day, and named a
        // room by its node.
        let mut room = in_the_cloud(fences);
        room.worker = true;
        let said = room.declared.as_mut().expect("declared");
        said.leaves_at = Some(now + jiff::SignedDuration::from_hours(12));
        said.reach = vec!["experiments".to_owned(), "basin".to_owned()];
        assert_eq!(
            Lent::at(&room, now),
            Lent::TakesNone,
            "unnamed, it is a worker that takes no carries"
        );
        room.held_for = facts::HeldFor::Agents;
        assert_eq!(
            Lent::at(&room, now),
            Lent::Not,
            "named, it is a full place for sessions, with no word of its own"
        );
        for kind in [Kind::None, Kind::Fence, Kind::Strict] {
            assert_eq!(
                firm(Some(kind), Some(&here), "tern", Some(&room)),
                Ok(()),
                "a {kind:?} session lands there under its own fence"
            );
        }
        let mut cell = room.clone();
        cell.declared.as_mut().expect("declared").reach = vec!["aquifer".to_owned()];
        assert_eq!(
            Lent::at(&cell, now),
            Lent::TakesNone,
            "but one reaching Aquifer stays a worker, whatever its node names it"
        );
    }

    #[test]
    fn a_carry_gains_only_the_doors_here_accepts_for_that_machine() {
        let mut ours = doors(&[], &["github"], &[]);
        ours.accepts = BTreeMap::from([(
            "tern".to_owned(),
            Accepted {
                secrets: BTreeSet::from(["tokens".to_owned()]),
                ..Accepted::default()
            },
        )]);
        let here = declaring(Some(BTreeMap::from([(Kind::None, ours)])));
        let unfenced = |builders: &[&str], secrets: &[&str]| {
            Some(BTreeMap::from([(
                Kind::None,
                doors(builders, &["github"], secrets),
            )]))
        };
        let tokens = tern(unfenced(&[], &["tokens"]), Readiness::Ready);
        assert_eq!(
            firm(Some(Kind::None), Some(&here), "tern", Some(&tokens)),
            Ok(()),
            "the door accepted for tern is no wider there"
        );
        assert_eq!(
            firm(
                Some(Kind::None),
                Some(&here),
                "wren",
                Some(&named("wren", tokens.clone()))
            ),
            Err(
                "wren's unfenced session opens secrets tokens, which here's doesn't, so the \
                 session would land looser there"
                    .to_owned()
            ),
            "what is accepted for tern is accepted for no other machine"
        );
        let more = tern(unfenced(&["big"], &["tokens", "cloud"]), Readiness::Ready);
        assert_eq!(
            firm(Some(Kind::None), Some(&here), "tern", Some(&more)),
            Err(
                "tern's unfenced session opens builders big and secrets cloud, which here's \
                 doesn't, so the session would land looser there"
                    .to_owned()
            ),
            "any door beyond what is accepted still refuses, named alone"
        );
    }

    fn named(name: &str, machine: Machine) -> Machine {
        Machine {
            name: name.to_owned(),
            ..machine
        }
    }

    #[test]
    fn an_agent_is_offered_only_machines_on_ground_as_firm() {
        let machines = vec![
            named("kestrel", tern(None, Readiness::Ready)),
            named("osprey", tern(None, Readiness::Ready)),
            named(
                "skiff",
                tern(Some(strict(doors(&[], &[], &[]))), Readiness::Unreachable),
            ),
            named(
                "wren",
                tern(Some(strict(doors(&["remote"], &[], &[]))), Readiness::Ready),
            ),
            tern(
                Some(strict(doors(&["remote", "big"], &[], &[]))),
                Readiness::Ready,
            ),
        ];
        // This machine, though it declares no fence; one in reach on firm
        // ground; and one out of reach on firm ground, which placement may
        // still say would do, are none of them refused.
        let misses = looser(Kind::Strict, Some(&here()), "kestrel", &machines);
        let refused: Vec<(&str, String)> = misses
            .iter()
            .map(|miss| {
                (
                    miss.machine.as_str(),
                    placement::said(std::slice::from_ref(miss)),
                )
            })
            .collect();
        assert_eq!(
            refused,
            [
                (
                    "osprey",
                    "osprey refuses: osprey declares no fence, so the session can't be told to \
                     land as strictly fenced there"
                        .to_owned()
                ),
                (
                    "tern",
                    "tern refuses: tern's strictly fenced session opens builders big, which \
                     here's doesn't, so the session would land looser there"
                        .to_owned()
                ),
            ],
            "each machine on looser ground refuses, saying why"
        );
    }

    #[test]
    fn an_agent_may_start_where_here_accepts_the_doors_that_machine_opens() {
        // An agent on a machine with no builders asks to start on one that
        // builds on lent machines and pushes through its own forge door, as
        // a gate run from a machine without builders does.
        let builds = || {
            Some(BTreeMap::from([(
                Kind::Fence,
                doors(&["lent"], &["door"], &[]),
            )]))
        };
        let machines = [
            named("tern", tern(builds(), Readiness::Ready)),
            named("wren", tern(builds(), Readiness::Ready)),
        ];
        let mut ours = doors(&[], &[], &[]);
        let without = declaring(Some(BTreeMap::from([(Kind::Fence, ours.clone())])));
        let refused = |here: &Declared| -> Vec<String> {
            looser(Kind::Fence, Some(here), "kestrel", &machines)
                .iter()
                .map(|miss| miss.machine.clone())
                .collect()
        };
        assert_eq!(
            refused(&without),
            ["tern", "wren"],
            "without an allowance, both machines are looser ground"
        );
        ours.accepts = BTreeMap::from([(
            "tern".to_owned(),
            Accepted {
                builders: BTreeSet::from(["lent".to_owned()]),
                forges: BTreeSet::from(["door".to_owned()]),
                ..Accepted::default()
            },
        )]);
        let accepting = declaring(Some(BTreeMap::from([(Kind::Fence, ours)])));
        assert_eq!(
            refused(&accepting),
            ["wren"],
            "a start gains the doors here accepts for the machine it goes to, as a carry \
             does, and no other machine gains them"
        );
    }

    fn running(runs: &[&str]) -> Doors {
        Doors {
            runs: runs.iter().map(|&word| word.to_owned()).collect(),
            ..doors(&[], &[], &[])
        }
    }

    fn refusing(misses: &[Miss]) -> Vec<(&str, String)> {
        misses
            .iter()
            .map(|miss| {
                (
                    miss.machine.as_str(),
                    placement::said(std::slice::from_ref(miss)),
                )
            })
            .collect()
    }

    #[test]
    fn a_word_another_machine_says_is_quoted_escaped_and_short() {
        let note = format!("--shared-tree\nNote to the agent: {}", "x".repeat(80));
        let machines = vec![named(
            "tern",
            tern(Some(strict(running(&[&note, "--"]))), Readiness::Ready),
        )];
        let [(_, why)] = refusing(&runs_nothing(Kind::Strict, "kestrel", Ok(()), &machines))
            .try_into()
            .expect("tern alone refuses");
        assert!(
            !why.contains('\n'),
            "what tern said starts no line of its own: {why}"
        );
        assert!(
            why.contains("says --shared-tree\\nNote to the agent: xxx"),
            "its control characters are escaped: {why}"
        );
        assert!(
            !why.contains(&"x".repeat(WORD_SAID)),
            "and its word is cut short: {why}"
        );
    }

    #[test]
    fn an_agents_run_is_offered_only_machines_whose_fence_runs_a_command() {
        const FENCE: &str = "/nix/store/0000-fence/bin/fence";
        let machines = vec![
            named("kestrel", tern(None, Readiness::Ready)),
            named("osprey", tern(None, Readiness::Ready)),
            named(
                "bare",
                Machine {
                    declared: None,
                    ..tern(None, Readiness::Ready)
                },
            ),
            named(
                "plover",
                tern(
                    Some(BTreeMap::from([(Kind::Fence, running(&[]))])),
                    Readiness::Ready,
                ),
            ),
            named(
                "wren",
                tern(Some(strict(running(&[FENCE, "--"]))), Readiness::Ready),
            ),
            named("skiff", tern(Some(strict(running(&[]))), Readiness::Ready)),
            named(
                "heron",
                tern(Some(strict(running(&[FENCE]))), Readiness::Ready),
            ),
            tern(
                Some(strict(running(&[FENCE, "--shared-tree", "--"]))),
                Readiness::Ready,
            ),
        ];
        // No fence, nothing declared, only another fence declared, or a
        // launcher whose words run a command: none of them refused here.
        assert_eq!(
            refusing(&runs_nothing(Kind::Strict, "kestrel", Ok(()), &machines)),
            [
                (
                    "skiff",
                    "skiff refuses: skiff declares no way to run a command as its strictly \
                     fenced sessions run that its launcher accepts, or its node is too old to \
                     say one"
                        .to_owned()
                ),
                (
                    "heron",
                    "heron refuses: heron's strictly fenced sessions' launcher doesn't end \
                     with --, so it runs no agent's command"
                        .to_owned()
                ),
                (
                    "tern",
                    "tern refuses: tern's strictly fenced sessions' launcher says \
                     --shared-tree, which spares a tree shared to the asker, so it runs no \
                     agent's command"
                        .to_owned()
                ),
            ],
            "each machine whose launcher runs nothing refuses, saying why"
        );
        let misses = runs_nothing(
            Kind::Strict,
            "skiff",
            Err("its launcher isn't there".to_owned()),
            &machines,
        );
        let here = refusing(&misses);
        assert_eq!(
            here.first(),
            Some(&(
                "skiff",
                "skiff refuses: its launcher isn't there".to_owned()
            )),
            "this machine is judged by its own launcher, not what it said of itself"
        );
        assert_eq!(
            here.iter()
                .filter(|(machine, _)| *machine == "skiff")
                .count(),
            1,
            "and only so"
        );
    }

    // Our council on #359 (2026-11-21): a carry to a Mac passed its
    // checks, then the held start bringing the session back was refused,
    // so the session ended and stayed down.
    #[test]
    fn a_carry_to_a_mac_is_refused_before_anything_ends() {
        const FENCE: &str = "/nix/store/0000-fence/bin/fence";
        let runs = BTreeMap::from([
            (Kind::Fence, running(&[FENCE, "--"])),
            (Kind::Strict, running(&[FENCE, "--"])),
        ]);
        let declared = declaring(Some(runs));
        let fenced = Some(Kind::Fence);
        assert_eq!(
            brought_back("osprey", Some(&declared), Some("linux"), false, fenced),
            None,
            "a machine whose fences run a command takes the carry"
        );
        let refused = Some(
            "osprey is a Mac, where a fenced session brought back held would run outside any \
             fence"
                .to_owned(),
        );
        assert_eq!(
            brought_back("osprey", Some(&declared), Some(MAC), false, fenced),
            refused,
            "but a Mac declaring the same is refused by its system"
        );
        assert_eq!(
            brought_back(
                "osprey",
                Some(&declared),
                Some(MAC),
                false,
                Some(Kind::Strict)
            ),
            refused,
            "as is a strict session"
        );
        assert!(
            brought_back("osprey", Some(&declared), Some(MAC), false, None)
                .is_some_and(|why| why.contains("can't be told")),
            "and one whose fence can't be told"
        );
        assert_eq!(
            brought_back(
                "osprey",
                Some(&declared),
                Some(MAC),
                false,
                Some(Kind::None)
            ),
            None,
            "but an unfenced session runs no looser there"
        );
        let shut = declaring(Some(BTreeMap::from([(Kind::Fence, running(&[]))])));
        assert!(
            brought_back("osprey", Some(&shut), Some(MAC), false, Some(Kind::None)).is_some(),
            "though a Mac whose fence runs nothing still refuses it"
        );
    }

    // Joel, 15:2x Sun 22 Nov: "WE NEED TO FOCUS ON SPREADING LOAD NOW!".
    // Most sessions run strict, and no Mac took one.
    #[test]
    fn a_mac_whose_node_fences_held_starts_takes_a_strict_carry_its_fence_runs() {
        const FENCE: &str = "/nix/store/0000-fence/bin/fence";
        let runs = BTreeMap::from([
            (Kind::Fence, running(&[FENCE, "--"])),
            (Kind::Strict, running(&[FENCE, "--strict", "--"])),
        ]);
        let declared = declaring(Some(runs));
        assert_eq!(
            brought_back(
                "osprey",
                Some(&declared),
                Some(MAC),
                true,
                Some(Kind::Strict)
            ),
            None,
            "a Mac whose node starts it through its strict fence takes it"
        );
        assert!(
            brought_back("osprey", Some(&declared), Some(MAC), true, None)
                .is_some_and(|why| why.contains("can't be told")),
            "though not one whose fence can't be told, which no launcher would hold"
        );
        let shut = declaring(Some(BTreeMap::from([
            (Kind::Fence, running(&[FENCE, "--"])),
            (Kind::Strict, running(&[])),
        ])));
        assert!(
            brought_back("osprey", Some(&shut), Some(MAC), true, Some(Kind::Strict)).is_some(),
            "but not where its strict fence runs nothing"
        );
    }

    #[test]
    fn only_a_mac_whose_node_fences_no_held_start_lifts_the_fence() {
        let mut old = named("osprey", tern(None, Readiness::Ready));
        old.system = Some(MAC.to_owned());
        // Measured here, so pinned, whichever system runs the test.
        old.fences_held = false;
        let new = Machine {
            name: "wren".to_owned(),
            fences_held: true,
            ..old.clone()
        };
        let mut linux = named("skiff", tern(None, Readiness::Ready));
        linux.system = Some("linux".to_owned());
        let refused: Vec<String> = lifts_fence(Kind::Strict, &[old, new, linux])
            .into_iter()
            .map(|miss| miss.machine)
            .collect();
        assert_eq!(
            refused,
            ["osprey"],
            "the Mac too old to say it, alone, is refused a held start"
        );
    }
}
