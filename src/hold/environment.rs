//! The person's environment as it stands now, which a holder starts its
//! program in at every start, over the environment the holder itself was
//! started with.
//!
//! A holder can be started long before its program: by a node at login,
//! or by `mycelium hold` from a window whose opener began hours ago. Its
//! own environment is that start's, so a variable the person's
//! configuration gained since, such as a new default model, would never
//! reach a program it starts. On Linux the person's service manager keeps
//! the environment as their session has it now, and it is read afresh at
//! each start, the first and every one after a park. PATH is the one the
//! holder's own keeps first ([`keeping_path`]), since the shell it was
//! started from adds folders the manager never hears of.

use std::process::Command;

/// Variables a holder keeps from its own environment, since they say
/// where the shell or the terminal that started it stands rather than
/// what the person's session is: the service manager's would name its
/// own folder, its own shell depth and its own terminal.
const KEPT: [&str; 5] = ["PWD", "OLDPWD", "SHLVL", "_", "TERM"];

/// How long the service manager has to answer before a start goes on
/// with the holder's own environment.
const ANSWERS_WITHIN: &str = "--timeout=2";

/// Where a holder learns the person's environment as it is now.
pub trait Environment: Send + Sync {
    /// Each variable and its value, as the person's session has them now;
    /// none when that can't be known, so a start goes on with the
    /// holder's own.
    fn now(&self) -> Vec<(String, String)>;
}

/// The person's service manager: systemd's user manager on Linux, read
/// over D-Bus with `busctl`. A Mac has none that lists it, so a holder
/// there starts its program in its own environment.
pub struct ServiceManager;

impl Environment for ServiceManager {
    fn now(&self) -> Vec<(String, String)> {
        if !cfg!(target_os = "linux") {
            return Vec::new();
        }
        let answer = Command::new("busctl")
            .args([
                "--user",
                ANSWERS_WITHIN,
                "--json=short",
                "get-property",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "Environment",
            ])
            .output();
        // A manager that doesn't answer leaves the holder's own, as a
        // start had before it was asked.
        match answer {
            Ok(answer) if answer.status.success() => listed(&answer.stdout),
            _ => Vec::new(),
        }
    }
}

/// The variables `busctl --json=short` says the manager's `Environment`
/// property holds, `{"type":"as","data":["NAME=value", ...]}`, leaving
/// out any it can't read and those a holder keeps its own of.
fn listed(answer: &[u8]) -> Vec<(String, String)> {
    #[derive(serde::Deserialize)]
    struct Property {
        data: Vec<String>,
    }
    let Ok(property) = serde_json::from_slice::<Property>(answer) else {
        return Vec::new();
    };
    property
        .data
        .into_iter()
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            (!name.is_empty() && !KEPT.contains(&name)).then(|| (name.to_owned(), value.to_owned()))
        })
        .collect()
}

/// The person's variables (`person`, as [`Environment::now`] gives them)
/// with the holder's own PATH (`own`) kept first, and only the folders of
/// the person's it lacks after it. The shell or terminal a holder starts
/// from puts on PATH what the service manager never hears of, such as a
/// developer's own tools, which a program's start may run; a folder the
/// person's configuration gained since still arrives, after them.
pub fn keeping_path(person: Vec<(String, String)>, own: Option<&str>) -> Vec<(String, String)> {
    let Some(own) = own.filter(|own| !own.is_empty()) else {
        return person;
    };
    let kept: Vec<&str> = own.split(':').collect();
    person
        .into_iter()
        .map(|(name, value)| {
            if name != "PATH" {
                return (name, value);
            }
            let gained = value
                .split(':')
                .filter(|folder| !folder.is_empty() && !kept.contains(folder));
            let joined = kept.iter().copied().chain(gained).collect::<Vec<_>>();
            (name, joined.join(":"))
        })
        .collect()
}

/// An environment that answers what it was given, for a holder's tests.
#[cfg(test)]
pub struct Given(pub Vec<(&'static str, &'static str)>);

#[cfg(test)]
impl Environment for Given {
    fn now(&self) -> Vec<(String, String)> {
        self.0
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_managers_variables_are_read_each_whole() {
        let answer = br#"{"type":"as","data":["PI_MODEL=x","MODEL=a=b","LANG=","bare","=x"]}"#;
        assert_eq!(
            listed(answer),
            [("PI_MODEL", "x"), ("MODEL", "a=b"), ("LANG", "")]
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .to_vec(),
            "each NAME=value is read with its value whole, and a line without a name is left out"
        );
    }

    #[test]
    fn where_the_holders_shell_stands_is_its_own() {
        let answer = br#"{"type":"as","data":["PWD=/","OLDPWD=/","SHLVL=1","_=/bin/sh","TERM=linux","EDITOR=vi"]}"#;
        assert_eq!(
            listed(answer),
            vec![("EDITOR".to_owned(), "vi".to_owned())],
            "the manager's folder, shell depth and terminal are left to the holder's own"
        );
    }

    fn variables(given: &[(&str, &str)]) -> Vec<(String, String)> {
        given
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_holders_own_path_comes_first_and_gains_the_persons_new_folders() {
        let person = variables(&[("PATH", "/profile/bin:/run/bin"), ("EDITOR", "vi")]);
        assert_eq!(
            keeping_path(person, Some("/dev/bin:/run/bin")),
            variables(&[("PATH", "/dev/bin:/run/bin:/profile/bin"), ("EDITOR", "vi")]),
            "the folders the holder's shell put on its PATH stay, first, and the person's it \
             lacks come after them, each once"
        );
    }

    #[test]
    fn a_holder_without_a_path_takes_the_persons() {
        let person = variables(&[("PATH", "/profile/bin")]);
        assert_eq!(
            keeping_path(person.clone(), None),
            person,
            "with no PATH of its own, the holder's program has the person's"
        );
    }

    #[test]
    fn a_person_without_a_path_leaves_the_holders() {
        let person = variables(&[("EDITOR", "vi")]);
        assert_eq!(
            keeping_path(person.clone(), Some("/dev/bin")),
            person,
            "nothing is said of PATH, so the program keeps the holder's own"
        );
    }

    #[test]
    fn an_answer_that_cant_be_read_gives_nothing() {
        assert!(
            listed(b"Failed to connect to bus").is_empty(),
            "a start goes on with the holder's own environment"
        );
    }
}
