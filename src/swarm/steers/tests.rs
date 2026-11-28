//! Steers aimed at the harbour's swarm, and what each says back.

use serde_json::json;

use super::{Act, Aim, Aimed, aim, told};
use crate::forage::one_line;
use crate::swarm::tests::swarm;

/// The addresses `aim` tells, in order.
fn addresses(aim: &Aim) -> Vec<&str> {
    aim.sessions
        .iter()
        .map(|aimed| aimed.address.as_str())
        .collect()
}

fn say(words: &str) -> Act {
    Act::Say {
        words: words.to_owned(),
        now: false,
    }
}

#[test]
fn words_to_a_team_go_to_its_lead_from_the_person() {
    let aimed = aim(&swarm(), say("Ship it"), "harbour").expect("aimed");
    assert_eq!(
        aimed.sessions,
        [Aimed {
            address: "LEAD".to_owned(),
            name: "Harbour's lead".to_owned(),
            steer: json!({"text": "Ship it", "from": "person"}),
        }],
        "its lead is told, as the person"
    );
    assert_eq!(
        told(&aimed, &[Ok("skiff".to_owned())]),
        Ok("said to Harbour's lead, who leads Harbour".to_owned()),
        "and says who heard it"
    );
    assert_eq!(
        aim(&swarm(), say("Hello"), "reef01"),
        Err("Reef has no lead to say it to".to_owned()),
        "a team with no lead is said to be one"
    );
}

// Joel chose that words to a working session wait for its turn to end
// unless he says now (2026-11-12): the steer says which, and so does
// what's told back.
#[test]
fn words_said_now_go_into_the_turn_and_say_so() {
    let now = Act::Say {
        words: "Stop that".to_owned(),
        now: true,
    };
    let aimed = aim(&swarm(), now, "harbour").expect("aimed");
    assert_eq!(
        aimed.sessions[0].steer,
        json!({"text": "Stop that", "from": "person", "now": true}),
        "the steer says now"
    );
    assert_eq!(
        told(&aimed, &[Ok("skiff".to_owned())]),
        Ok("said now to Harbour's lead, who leads Harbour".to_owned()),
        "and so does what's told back"
    );
}

#[test]
fn a_pause_reaches_every_session_of_a_team_and_those_within_not_paused_already() {
    let aimed = aim(&swarm(), Act::Pause, "Harbour").expect("aimed");
    assert_eq!(
        addresses(&aimed),
        ["LEAD", "BUSY", "ASK"],
        "its own, then Kelp's within it, the paused one left"
    );
    assert_eq!(
        aimed.sessions[0].steer,
        json!({"act": "pause", "from": "person"}),
        "each told to pause, as the person"
    );
    assert_eq!(
        told(
            &aimed,
            &[
                Ok("skiff".to_owned()),
                Err("no session goes by BUSY now".to_owned()),
                Ok("skiff".to_owned()),
            ]
        ),
        Err("paused Harbour's lead and Kelp's question, in Harbour\n\
             Busy one wasn't told: no session goes by BUSY now"
            .to_owned()),
        "one not told is said, and why"
    );
}

#[test]
fn what_a_steer_says_back_holds_peers_words_to_one_line() {
    let (lead, busy, why) = (
        "Harbour's \u{1b}]0;x\u{7}lead",
        "Busy\r\none",
        "tern said \u{1b}[2Jgone\nlater",
    );
    let mut dirty = swarm();
    for session in &mut dirty.sessions {
        if session["id"] == "s-lead" {
            session["title"] = json!(format!("{lead}\u{fdd0}"));
        }
        if session["id"] == "s-busy" {
            session["title"] = json!(busy);
        }
    }
    let aimed = aim(&dirty, Act::Pause, "Harbour").expect("aimed");
    let said = told(
        &aimed,
        &[
            Ok("skiff".to_owned()),
            Err(why.to_owned()),
            Ok("skiff".to_owned()),
        ],
    );
    assert_eq!(
        said,
        Err(format!(
            "paused {} and Kelp's question, in Harbour\n{} wasn't told: {}",
            one_line(lead),
            one_line(busy),
            one_line(why)
        )),
        "a session's title, or the node's reason, drew otherwise than on one line"
    );
}

#[test]
fn a_steer_finds_a_team_by_the_name_its_page_shows() {
    let mut short = swarm();
    short.teams[1]["swarm"] = json!("do\u{200B}ck\u{fdd0}");
    let mut titled = swarm();
    titled.teams[1]["title"] = json!("Harb\u{200B}ou\u{fdd0}r");
    for (swarm, asked) in [(short, "dock"), (titled, "arbou")] {
        let aimed = aim(&swarm, Act::Pause, asked)
            .unwrap_or_else(|why| panic!("{asked}, as the page shows it, missed: {why}"));
        assert_eq!(
            addresses(&aimed),
            ["LEAD", "BUSY", "ASK"],
            "{asked} steered another than Harbour"
        );
    }
}

#[test]
fn a_short_name_never_takes_a_session_s_address() {
    let mut swarm = swarm();
    swarm.teams[0]["swarm"] = json!("busy");
    let say = Act::Say {
        words: "Stop that".to_owned(),
        now: false,
    };
    let aimed = aim(&swarm, say, "BUSY").expect("aimed");
    assert_eq!(
        addresses(&aimed),
        ["BUSY"],
        "a team named for a session's address took its words"
    );
}

#[test]
fn a_peer_s_team_under_a_session_s_address_never_takes_its_words() {
    // A team is keyed by what its members say orchestrates them, so
    // a peer may key one by a working session's address and lead it.
    let mut swarm = swarm();
    swarm.teams[1]["key"] = json!("team:BUSY");
    swarm.teams[2]["within"] = json!("BUSY");
    let say = Act::Say {
        words: "Stop that".to_owned(),
        now: false,
    };
    let aimed = aim(&swarm, say, "BUSY").expect("aimed");
    assert_eq!(
        addresses(&aimed),
        ["BUSY"],
        "a team led by another took the words said to a session"
    );
}

#[test]
fn a_team_goes_by_its_lead_s_address() {
    let whole = aim(&swarm(), Act::Pause, "HARBOR").expect("aimed");
    let mut swarm = swarm();
    swarm.teams[1]["key"] = json!("team:LEAD");
    swarm.teams[2]["within"] = json!("LEAD");
    let aimed = aim(&swarm, Act::Pause, "LEAD").expect("aimed");
    assert_eq!(
        addresses(&aimed),
        addresses(&whole),
        "an orchestrator's team goes by its address, as it leads it"
    );
    // A second session saying the lead's address, found before it,
    // still leaves the steer to the team the lead leads.
    let mut twin = swarm.sessions[0].clone();
    twin["id"] = json!("s-twin");
    twin["address"] = json!("LEAD");
    swarm.sessions.insert(0, twin);
    let aimed = aim(&swarm, Act::Pause, "LEAD").expect("aimed");
    assert_eq!(
        addresses(&aimed),
        addresses(&whole),
        "a session saying its lead's address took the team's steer"
    );
}

#[test]
fn a_team_s_own_address_is_never_hidden_by_a_short_name_drawn_like_it() {
    let whole = aim(&swarm(), Act::Pause, "HARBOR").expect("aimed");
    let mut swarm = swarm();
    swarm.teams[1]["swarm"] = json!("harbor");
    swarm.teams[0]["swarm"] = json!("har\u{200B}bor");
    let aimed = aim(&swarm, Act::Pause, "HARBOR").expect("its address reaches it");
    assert_eq!(
        addresses(&aimed),
        addresses(&whole),
        "a peer's short name cleaned to a team's address hid it"
    );
}

#[test]
fn two_teams_whose_titles_clean_alike_are_each_named() {
    // Short names that clean alike go to neither, so two teams are
    // drawn alike only by titles that clean alike.
    let mut swarm = swarm();
    swarm.teams[1]["title"] = json!("Dock");
    swarm.teams[0]["title"] = json!("Do\u{200B}ck");
    let why =
        aim(&swarm, Act::Pause, "dock").expect_err("a steer to two teams drawn alike is refused");
    assert_eq!(
        why, "dock could be Dock (REEF01) or Dock (HARBOR)",
        "each is named with what to type to reach it"
    );
    // `swarm` takes the first act it finds as the act, so an act's
    // own word typed before it would do that act instead.
    swarm.teams[0]["key"] = json!("team:RESUME");
    assert_eq!(
        aim(&swarm, Act::Pause, "dock"),
        Err("dock could be Dock (no address to type) or Dock (HARBOR)".to_owned()),
        "an act's word was given as an address to type"
    );
    swarm.teams[0]["key"] = json!("team:ALONE");
    assert_eq!(
        aim(&swarm, Act::Pause, "dock"),
        Err("dock could be Dock (no address to type) or Dock (HARBOR)".to_owned()),
        "an address that would steer a session instead is not given"
    );
    // A peer's key holding a letter a keyboard doesn't type, here
    // Cyrillic Ie drawn as an E, is no address, so none is given.
    swarm.teams[0]["key"] = json!("team:R\u{415}EF01");
    assert_eq!(
        aim(&swarm, Act::Pause, "dock"),
        Err("dock could be Dock (no address to type) or Dock (HARBOR)".to_owned()),
        "a key holding a lookalike was given as an address to type"
    );
}

#[test]
fn a_team_keyed_like_another_but_for_case_never_hides_it() {
    let whole = aim(&swarm(), Act::Pause, "HARBOR").expect("aimed");
    let mut swarm = swarm();
    swarm.teams[0]["key"] = json!("team:harbor");
    assert_eq!(
        addresses(&aim(&swarm, Act::Pause, "HARBOR").expect("its own case reaches it")),
        addresses(&whole),
        "a team keyed alike but for case hid Harbour"
    );
    assert_eq!(
        addresses(&aim(&swarm, Act::Pause, "harbor").expect("its own case reaches it")),
        ["CORAL"],
        "the other was hidden in turn"
    );
}

#[test]
fn two_sessions_named_alike_are_each_given_their_address() {
    let mut swarm = swarm();
    swarm.sessions[5]["title"] = json!("Busy one");
    assert_eq!(
        aim(&swarm, Act::Pause, "busy one"),
        Err("busy one could be Busy one (BUSY) or Busy one (ALONE)".to_owned()),
        "each session's own address reaches it, so each is given"
    );
}

#[test]
fn a_resume_reaches_only_the_paused() {
    let aimed = aim(&swarm(), Act::Resume, "harbour").expect("aimed");
    assert_eq!(addresses(&aimed), ["PAUSE"], "only the paused one");
    assert_eq!(
        told(&aimed, &[Ok("skiff".to_owned())]),
        Ok("resumed Paused one, in Harbour".to_owned()),
        "said back by its name"
    );
    assert_eq!(
        aim(&swarm(), Act::Resume, "REEF01"),
        Err("none of Reef's sessions is paused".to_owned()),
        "a team with none paused says so"
    );
}

#[test]
fn one_session_goes_by_its_address_or_its_name() {
    let aimed = aim(&swarm(), say("Hi"), "alone").expect("aimed");
    assert_eq!(
        (addresses(&aimed), &aimed.team),
        (vec!["ALONE"], &None),
        "by its address"
    );
    assert_eq!(
        told(&aimed, &[Ok("skiff".to_owned())]),
        Ok("said to On its own".to_owned()),
        "said back by its name alone"
    );
    let busy = aim(&swarm(), Act::Pause, "busy one").expect("aimed");
    assert_eq!(addresses(&busy), ["BUSY"], "by a part of its name");
    assert_eq!(
        aim(&swarm(), Act::Pause, "paused one"),
        Err("Paused one is paused already".to_owned()),
        "a pause that changes nothing is refused"
    );
}

#[test]
fn a_name_matching_nothing_or_several_is_said_plainly() {
    assert_eq!(
        aim(&swarm(), Act::Pause, "tide"),
        Err("nothing in the swarm is named tide".to_owned()),
        "none"
    );
    assert_eq!(
        aim(&swarm(), Act::Pause, "one"),
        Err("one could be Paused one, Busy one or Reef's one".to_owned()),
        "several living, the ended one left out"
    );
    assert_eq!(
        aim(&swarm(), Act::Pause, "e"),
        Err("e could be reef or kelp".to_owned()),
        "several teams, each by its short name"
    );
}
