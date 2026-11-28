//! How the lend stands, for the node's `health`: whether its file is
//! taken and its cuts can be read, as they are when asked, so a file
//! fixed reads answering at once rather than once a machine next asks.

use serde_json::json;

use super::{EVERY, Lender, cuts};

impl Lender {
    /// How the lend stands, as the daemon interface's `health` names a
    /// source: `refused` with why while its file isn't taken, its cuts
    /// can't be read or every machine is cut off, and `answering`
    /// otherwise. Read afresh at each ask and said nowhere else, since
    /// health is asked often and the log already has each lend's why.
    #[must_use]
    pub fn health(&self) -> serde_json::Value {
        let why = match (self.read(), cuts(&self.folder)) {
            (Err(why), _) => Some(format!("its file isn't taken: {why}")),
            (Ok(_), Err(error)) => Some(format!(
                "its cuts can't be read, so it lends nothing: {error}"
            )),
            (Ok(_), Ok(cut)) if cut.iter().any(|cut| cut == EVERY) => {
                Some("every machine is cut off from it".to_owned())
            }
            (Ok(_), Ok(_)) => None,
        };
        let file = self.file.display().to_string();
        match why {
            Some(why) => json!({"name": "lend", "file": file, "state": "refused", "why": why}),
            None => json!({"name": "lend", "file": file, "state": "answering"}),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use serde_json::json;

    use super::super::{Cut, Lender, cut};

    /// Longer than reading a lend should take, so a test fails rather
    /// than hangs.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// A lend that may be taken, lending nothing anyone asks yet.
    fn taken() -> serde_json::Value {
        json!({
            "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 3}],
            "budgets": {"models": {
                "upstream": "relay/agents.sock",
                "host": "models.example",
                "paths": ["/v1/messages"],
                "auth_header": "authorization",
                "key": ["/bin/sh", "-c", "printf 'Bearer lent-key\\n'"],
            }},
        })
    }

    fn state(lender: &Lender) -> String {
        let health = lender.health();
        assert_eq!(health["name"], "lend", "{health}");
        health["state"].as_str().expect("its state").to_owned()
    }

    #[test]
    fn a_lend_says_why_it_isnt_taken_for_as_long_as_it_isnt() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("lend.json");
        let lender = Lender::new(
            file.clone(),
            vec![0, 65534, rustix::process::getuid().as_raw()],
            folder.path().join("state"),
            Some(folder.path().to_owned()),
        );
        assert_eq!(state(&lender), "refused", "a lend file that isn't there");

        // Serde's own error would quote this string, which may be a key
        // pasted in the wrong place.
        fs::write(&file, "{\"vms\": \"sk-pasted-key\", \"budgets\": {}}")
            .expect("a broken lend is written");
        let health = lender.health();
        assert_eq!(health["state"], "refused", "{health}");
        let why = health["why"].as_str().expect("why");
        assert!(why.contains("isn't taken"), "{why}");
        assert!(
            !why.contains("sk-pasted"),
            "it never says the words found: {why}"
        );

        fs::write(&file, taken().to_string()).expect("the lend is fixed");
        assert_eq!(
            state(&lender),
            "answering",
            "fixed, it answers with nothing asked"
        );

        fs::remove_file(&file).expect("the lend is taken away");
        let made = std::process::Command::new("mkfifo")
            .arg(&file)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo made no pipe: {made}");
        // Read apart, so a pipe read by mistake fails the test rather than
        // hanging it.
        let (said, heard) = mpsc::channel();
        thread::spawn(move || said.send(state(&lender)));
        let stood = heard.recv_timeout(PATIENCE).expect("a pipe is never read");
        assert_eq!(stood, "refused", "a pipe is refused, not read");
    }

    #[test]
    fn a_lend_cut_off_from_every_machine_says_so() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("lend.json");
        fs::write(&file, taken().to_string()).expect("the lend is written");
        let cuts = folder.path().join("state");
        fs::create_dir(&cuts).expect("the cuts' folder");
        let lender = Lender::new(
            file,
            vec![0, 65534, rustix::process::getuid().as_raw()],
            cuts.clone(),
            Some(folder.path().to_owned()),
        );
        cut(&cuts, &Cut::Machine("agents-one".to_owned())).expect("one is cut");
        assert_eq!(
            state(&lender),
            "answering",
            "one machine cut leaves the rest"
        );
        cut(&cuts, &Cut::All).expect("all are cut");
        assert_eq!(state(&lender), "refused");
        fs::write(cuts.join("cuts.json"), "\"sk-pasted-key\"").expect("the cuts are broken");
        let health = lender.health();
        assert_eq!(
            health["state"], "refused",
            "cuts that can't be read lend nothing"
        );
        let why = health["why"].as_str().expect("why");
        assert!(
            !why.contains("sk-pasted"),
            "it never says the words found: {why}"
        );
    }
}
