//! Whether a lent builder runs, as its lender says. A lent builder is one
//! made for a while, which ends itself once idle, so it is never dialled:
//! a connection to it may be counted as work and keep it from ending. Its
//! lender keeps a health file instead, found by a template naming the
//! builder's host as `{host}`, such as `/run/{host}/health.json`, of the
//! shape `{"at", "state"}`: `at` in seconds since the epoch, rewritten
//! about every half minute, and `state` the lender's word for how its
//! builder stands, `none` while nothing is lent.

use std::path::PathBuf;

use serde::Deserialize;

/// How old a lender's word may be, in seconds, before it says nothing,
/// and its builder counts as gone.
const SAYS_FOR: i64 = 120;

/// How a lent builder stands, as its lender says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lent {
    /// Its lender runs it, standing as the lender's word says, such as
    /// `passing`.
    Running(String),
    /// Nothing runs it now, for the reason said.
    NotRunning(String),
    /// Nothing says, for the reason said.
    Unknown(String),
}

/// What a lender's health file says, as far as it is read.
#[derive(Deserialize)]
struct Health {
    at: i64,
    state: String,
}

/// The health file of `host` by `template`, or none for a host no path
/// may be made of.
fn file_of(template: &str, host: &str) -> Option<PathBuf> {
    if host.is_empty() || host.contains('/') || host.starts_with('.') {
        return None;
    }
    Some(PathBuf::from(template.replace("{host}", host)))
}

/// How the builder of `host` stands at `now`, in seconds since the epoch,
/// as its lender's health file by `template` says. Neither the file's path
/// nor why it couldn't be read is said, as either may say where things are
/// on this machine.
#[must_use]
pub fn lent(template: Option<&str>, host: &str, now: i64) -> Lent {
    let Some(template) = template else {
        return Lent::Unknown("nothing says where its lender keeps its health".to_owned());
    };
    let Some(file) = file_of(template, host) else {
        return Lent::Unknown("its host names no health file".to_owned());
    };
    let text = match std::fs::read(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Lent::NotRunning("its lender says nothing of it".to_owned());
        }
        Err(_) => return Lent::Unknown("its lender's health can't be read".to_owned()),
    };
    let Ok(health) = serde_json::from_slice::<Health>(&text) else {
        return Lent::Unknown("its lender's health isn't as a lender writes it".to_owned());
    };
    let old = now - health.at;
    if old > SAYS_FOR {
        return Lent::NotRunning(format!(
            "its lender last said how it stands {} minutes ago",
            old / 60
        ));
    }
    match health.state.as_str() {
        "none" => Lent::NotRunning("nothing is lent".to_owned()),
        state => Lent::Running(state.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lent_builder_runs_while_its_lender_says_so_lately() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let template = format!("{}/{{host}}.json", folder.path().display());
        let says = |host: &str, health: &str| {
            std::fs::write(folder.path().join(format!("{host}.json")), health)
                .expect("the health is written");
        };
        says(
            "kestrel",
            r#"{"at": 1000, "state": "passing", "builder": "x"}"#,
        );
        says("skiff", r#"{"at": 1000, "state": "none", "builder": null}"#);
        says("heron", "not json");
        let template = Some(template.as_str());
        assert_eq!(
            lent(template, "kestrel", 1_030),
            Lent::Running("passing".to_owned())
        );
        assert_eq!(
            lent(template, "kestrel", 1_000 + SAYS_FOR + 1),
            Lent::NotRunning("its lender last said how it stands 2 minutes ago".to_owned()),
            "a word gone stale says it's gone"
        );
        assert_eq!(
            lent(template, "skiff", 1_030),
            Lent::NotRunning("nothing is lent".to_owned())
        );
        assert_eq!(
            lent(template, "plover", 1_030),
            Lent::NotRunning("its lender says nothing of it".to_owned())
        );
        assert_eq!(
            lent(template, "heron", 1_030),
            Lent::Unknown("its lender's health isn't as a lender writes it".to_owned()),
            "its file's path isn't said"
        );
        assert!(matches!(lent(template, "..", 1_030), Lent::Unknown(_)));
        assert!(matches!(lent(None, "kestrel", 1_030), Lent::Unknown(_)));
    }
}
