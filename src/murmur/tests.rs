use super::*;
use crate::web::{Said as Saying, Web};

/// How long a test waits for the murmurs to hear a change.
const PATIENCE: Duration = Duration::from_secs(10);

/// A time to say things at, in whole seconds.
const NOW: i64 = 1_794_800_000;

fn murmur(words: &str) -> Saying {
    Saying::Murmur {
        words: words.to_owned(),
    }
}

fn tie(from: &str, to: &str) -> Saying {
    Saying::Tie {
        from: from.to_owned(),
        to: to.to_owned(),
        how: None,
    }
}

fn webs_of(scratch: &Path) -> Webs {
    Webs::new(scratch.to_owned(), "skiff".to_owned())
}

#[test]
fn a_machine_serves_only_its_own_murmurs_of_the_last_day_with_their_ties() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let harbour = Web::of(dir.path(), "QEST-20261112-HARBOR").expect("a web");
    let tide = Web::of(dir.path(), "QEST-20261112-TIDE").expect("a web");
    harbour
        .say(
            "skiff",
            Some("KP0L6A"),
            murmur("long ago"),
            NOW - KEPT_FOR - 1,
        )
        .expect("said");
    let landed = harbour
        .say("skiff", Some("KP0L6A"), murmur("Landed is slow"), NOW - 10)
        .expect("said");
    harbour
        .say(
            "kestrel",
            Some("FZ9MFJ"),
            murmur("the dock wakes late"),
            NOW - 9,
        )
        .expect("said");
    harbour
        .say("kestrel", None, tie(&landed.id, "mycelium"), NOW - 8)
        .expect("tied");
    harbour
        .say(
            "skiff",
            None,
            Saying::Settle {
                of: landed.id.clone(),
                how: "filed".to_owned(),
            },
            NOW - 7,
        )
        .expect("settled");
    tide.say("skiff", None, murmur("the tide turns"), NOW - 5)
        .expect("said");
    let mut webs = webs_of(dir.path());
    assert!(webs.read(), "first, it reads");
    let purposes = HashMap::from([("KP0L6A".to_owned(), "the murmur kind".to_owned())]);
    let records = records(webs.said_since(NOW - KEPT_FOR), &purposes);
    assert_eq!(
        records.keys().collect::<Vec<_>>(),
        [
            &format!("murmur:KP0L6A:{}", NOW - 10),
            &format!("murmur:skiff:{}", NOW - 5),
        ],
        "this machine's, of the last day, said by the session or the machine"
    );
    assert_eq!(
        records[&format!("murmur:KP0L6A:{}", NOW - 10)],
        json!({
            "key": format!("murmur:KP0L6A:{}", NOW - 10), "kind": "murmur",
            "heard": (NOW - 10) * 1000, "at": NOW - 10, "writer": "KP0L6A",
            "machine": "skiff", "text": "Landed is slow", "tied_to": ["mycelium"],
            "purpose": "the murmur kind", "settled": "filed",
        }),
        "another machine's tie is still its tie"
    );
    assert_eq!(
        records[&format!("murmur:skiff:{}", NOW - 5)]["purpose"],
        Value::Null,
        "a cell with no session says no purpose"
    );
    assert!(!webs.read(), "nothing new, nothing changed");
    let aged = NOW - 10 + KEPT_FOR;
    assert!(
        webs.ages_out(aged - 1, aged),
        "a day after it was said, it ages out"
    );
    assert!(!webs.ages_out(aged - 2, aged - 1), "but not before");
}

#[test]
fn a_cells_murmurs_of_one_second_are_each_kept_in_the_order_said() {
    let said = |seq: u64, text: &str| Said {
        cell: "KP0L6A".to_owned(),
        machine: "skiff".to_owned(),
        at: NOW,
        seq,
        text: text.to_owned(),
        tied_to: Vec::new(),
        settled: None,
    };
    let all = [said(3, "third"), said(1, "first"), said(2, "second")];
    let records = records(&all, &HashMap::new());
    let texts: Vec<(String, Value)> = records
        .into_iter()
        .map(|(key, record)| (key, record["text"].clone()))
        .collect();
    assert_eq!(
        texts,
        [
            (format!("murmur:KP0L6A:{NOW}"), json!("first")),
            (format!("murmur:KP0L6A:{NOW}.2"), json!("second")),
            (format!("murmur:KP0L6A:{NOW}.3"), json!("third")),
        ]
    );
}

#[test]
fn a_web_not_made_yet_is_watched_for_from_the_nearest_folder_there() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let scratch = dir.path().join("scratch");
    let mut webs = webs_of(&scratch);
    assert!(!webs.read(), "no scratch folder holds nothing");
    assert_eq!(
        webs.watched(),
        &BTreeSet::from([dir.path().to_owned()]),
        "it is watched for from the folder above"
    );
    let quest = scratch.join("QEST-20261112-HARBOR");
    fs::create_dir_all(&quest).expect("a quest's folder");
    assert!(!webs.read(), "a quest with no web holds nothing");
    assert_eq!(
        webs.watched(),
        &BTreeSet::from([scratch.clone(), quest.clone()]),
        "the scratch folder, and the quest's for its web"
    );
    Web::of(&scratch, "QEST-20261112-HARBOR")
        .expect("a web")
        .say("skiff", None, murmur("the tide turns"), NOW)
        .expect("said");
    assert!(webs.read(), "its first murmur is read");
    assert!(
        webs.watched()
            .contains(&web::events_in(&quest.join(web::FOLDER))),
        "{:?}",
        webs.watched()
    );
    fs::remove_dir_all(&quest).expect("the quest's folder goes");
    assert!(webs.read(), "its murmurs go with it");
    assert_eq!(webs.said_since(0).count(), 0);
}

#[tokio::test]
async fn a_murmur_said_is_told_and_held() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let murmurs = Murmurs::start(Some(dir.path().to_owned()), "skiff".to_owned());
    let mut told = murmurs.told();
    let now = now();
    let web = Web::of(dir.path(), "QEST-20261112-HARBOR").expect("a web");
    let held = || murmurs.records(now + 1, &HashMap::new());
    let heard = tokio::time::timeout(PATIENCE, async {
        // Said again until heard, as the first may land before the watch
        // of a web not made yet.
        while held().is_empty() {
            web.say("skiff", None, murmur("the tide turns"), now)
                .expect("said");
            let _ = tokio::time::timeout(Duration::from_secs(1), told.changed()).await;
        }
    })
    .await;
    assert!(heard.is_ok(), "the murmur is heard");
    assert_eq!(
        murmurs.health()[0]["state"],
        "answering",
        "{:?}",
        murmurs.health()
    );
}
