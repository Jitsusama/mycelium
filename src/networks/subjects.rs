//! A cell's subjects as the networks hold them: one text, a line between
//! each, said as a list. A cell may say [`super::MOST_SUBJECTS`], and
//! ten thousand cells are held within 30 MB, so each subject costs its
//! own text and a byte rather than a string of its own, whose 24 bytes
//! beside it were most of what a short subject held.

use serde::{Serialize, Serializer};

/// A cell's subjects, in the order it said them. None is empty or holds
/// a line: `made::subjects` passes those over, and [`Subjects::kept`]
/// does too, so a line between each never splits or hides a subject.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subjects(Box<str>);

impl Subjects {
    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.each().next().is_none()
    }

    /// The subjects `said`, in order, passing over any that is empty or
    /// holds a line, which the text between them couldn't keep apart.
    pub(super) fn kept(said: Vec<String>) -> Self {
        let kept: Vec<String> = said
            .into_iter()
            .filter(|subject| !subject.is_empty() && !subject.contains('\n'))
            .collect();
        Self(kept.join("\n").into_boxed_str())
    }

    /// Each subject, in the order said.
    pub fn each(&self) -> impl Iterator<Item = &str> {
        self.0.split('\n').filter(|subject| !subject.is_empty())
    }
}

impl Serialize for Subjects {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.each())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_are_said_as_the_list_they_were_given() {
        let said = Subjects::kept(vec!["mycelium src/node".to_owned(), "hubs".to_owned()]);
        assert_eq!(
            serde_json::to_value(&said).expect("subjects serialize"),
            serde_json::json!(["mycelium src/node", "hubs"]),
            "each subject is said in its order"
        );
        assert_eq!(
            said.each().collect::<Vec<_>>(),
            ["mycelium src/node", "hubs"],
            "and read back as each was given"
        );
    }

    #[test]
    fn no_subjects_are_said_as_none() {
        let none = Subjects::kept(Vec::new());
        assert!(none.is_empty(), "no subjects hold nothing");
        assert_eq!(none.each().count(), 0, "and say none");
        assert_eq!(
            serde_json::to_value(&none).expect("subjects serialize"),
            serde_json::json!([]),
            "an empty list, where a reader asks for it"
        );
    }

    #[test]
    fn a_subject_empty_or_holding_a_line_is_passed_over() {
        let said = Subjects::kept(vec![
            String::new(),
            "one\ntwo".to_owned(),
            "hubs".to_owned(),
            String::new(),
        ]);
        assert_eq!(
            said.each().collect::<Vec<_>>(),
            ["hubs"],
            "a line would split one into two, and an empty one vanish"
        );
        let blank = Subjects::kept(vec![String::new(), String::new()]);
        assert!(blank.is_empty(), "only empty ones say none");
        assert_eq!(
            serde_json::to_value(&blank).expect("subjects serialize"),
            serde_json::json!([]),
            "and nothing reads back"
        );
    }
}
