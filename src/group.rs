//! Groups, the patterns that hear them, and the routes from one to the
//! other.
//!
//! A group is named by dot-separated tokens, such as `quest.4uhc5q`; the
//! mesh's `#quest/4uhc5q` and a peer's own `@4UHC5Q` are written that way
//! as `quest.4uhc5q` and `peer.4uhc5q`. A pattern names groups the same
//! way, where `*` stands for any one token and a final `>` for one or more.
//! Routes keep patterns in a trie, so finding who hears a group costs the
//! depth of its name, not the number of listeners.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Why a name isn't a group or a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidName {
    name: String,
    reason: &'static str,
}

impl fmt::Display for InvalidName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a group's name: {}", self.name, self.reason)
    }
}

impl std::error::Error for InvalidName {}

/// A group's name: dot-separated tokens of lowercase letters, digits, `-`
/// and `_`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Group(String);

impl Group {
    /// Takes `name` as a group's, refusing wildcards and malformed tokens.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidName> {
        let name = name.into();
        check(&name, false)?;
        Ok(Self(name))
    }

    /// The name as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn tokens(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for Group {
    type Err = InvalidName;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::new(name)
    }
}

impl TryFrom<String> for Group {
    type Error = InvalidName;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(name)
    }
}

impl From<Group> for String {
    fn from(group: Group) -> Self {
        group.0
    }
}

/// A pattern of groups: a group's name in which `*` stands for any one
/// token and a final `>` for one or more.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Pattern(String);

impl Pattern {
    /// Takes `pattern` as a pattern of groups.
    pub fn new(pattern: impl Into<String>) -> Result<Self, InvalidName> {
        let pattern = pattern.into();
        check(&pattern, true)?;
        Ok(Self(pattern))
    }

    /// The pattern that hears every group.
    pub fn everything() -> Self {
        Self(">".to_owned())
    }

    /// Whether this pattern hears `group`.
    pub fn matches(&self, group: &Group) -> bool {
        let mut wanted = self.0.split('.');
        let mut given = group.tokens();
        loop {
            match (wanted.next(), given.next()) {
                // A final `>` hears the rest, and running out together is
                // every token heard.
                (Some(">"), Some(_)) | (None, None) => return true,
                (Some("*"), Some(_)) => {}
                (Some(token), Some(name)) if token == name => {}
                _ => return false,
            }
        }
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for Pattern {
    type Err = InvalidName;

    fn from_str(pattern: &str) -> Result<Self, Self::Err> {
        Self::new(pattern)
    }
}

impl TryFrom<String> for Pattern {
    type Error = InvalidName;

    fn try_from(pattern: String) -> Result<Self, Self::Error> {
        Self::new(pattern)
    }
}

impl From<Pattern> for String {
    fn from(pattern: Pattern) -> Self {
        pattern.0
    }
}

fn check(name: &str, wildcards: bool) -> Result<(), InvalidName> {
    let refuse = |reason| {
        Err(InvalidName {
            name: name.to_owned(),
            reason,
        })
    };
    if name.is_empty() {
        return refuse("it is empty");
    }
    let tokens: Vec<&str> = name.split('.').collect();
    for (index, token) in tokens.iter().enumerate() {
        let last = index + 1 == tokens.len();
        match *token {
            "" => return refuse("a token is empty"),
            "*" | ">" if !wildcards => return refuse("a group's name has no wildcards"),
            ">" if !last => return refuse("`>` may only end a pattern"),
            "*" | ">" => {}
            _ if token.bytes().all(is_token_byte) => {}
            _ => return refuse("a token holds only a-z, 0-9, `-` and `_`"),
        }
    }
    Ok(())
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
}

/// Listeners by the patterns they hear, kept in a trie of tokens.
#[derive(Debug)]
pub struct Routes<K> {
    root: Branch<K>,
}

#[derive(Debug)]
struct Branch<K> {
    exact: HashMap<String, Branch<K>>,
    any_one: Option<Box<Branch<K>>>,
    here: Vec<K>,
    rest: Vec<K>,
}

impl<K> Default for Branch<K> {
    fn default() -> Self {
        Self {
            exact: HashMap::new(),
            any_one: None,
            here: Vec::new(),
            rest: Vec::new(),
        }
    }
}

impl<K> Default for Routes<K> {
    fn default() -> Self {
        Self {
            root: Branch::default(),
        }
    }
}

impl<K: Clone + PartialEq> Routes<K> {
    /// Routes the groups `pattern` hears to `listener`.
    pub fn insert(&mut self, pattern: &Pattern, listener: K) {
        let mut branch = &mut self.root;
        for token in pattern.0.split('.') {
            match token {
                ">" => {
                    branch.rest.push(listener);
                    return;
                }
                "*" => branch = branch.any_one.get_or_insert_with(Box::default),
                _ => branch = branch.exact.entry(token.to_owned()).or_default(),
            }
        }
        branch.here.push(listener);
    }

    /// Stops routing anything to `listener`.
    pub fn remove(&mut self, listener: &K) {
        self.root.remove(listener);
    }

    /// Every listener that hears any of `groups`, each once.
    pub fn listeners<'a>(&self, groups: impl IntoIterator<Item = &'a Group>) -> Vec<K> {
        let mut found = Vec::new();
        for group in groups {
            let tokens: Vec<&str> = group.tokens().collect();
            self.root.collect(&tokens, &mut found);
        }
        found
    }
}

impl<K: Clone + PartialEq> Branch<K> {
    fn collect(&self, tokens: &[&str], found: &mut Vec<K>) {
        let Some((first, rest)) = tokens.split_first() else {
            add_all(found, &self.here);
            return;
        };
        add_all(found, &self.rest);
        if let Some(next) = self.exact.get(*first) {
            next.collect(rest, found);
        }
        if let Some(next) = &self.any_one {
            next.collect(rest, found);
        }
    }

    fn remove(&mut self, listener: &K) {
        self.here.retain(|held| held != listener);
        self.rest.retain(|held| held != listener);
        for branch in self.exact.values_mut() {
            branch.remove(listener);
        }
        self.exact.retain(|_, branch| !branch.is_empty());
        if let Some(branch) = &mut self.any_one {
            branch.remove(listener);
            if branch.is_empty() {
                self.any_one = None;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.here.is_empty()
            && self.rest.is_empty()
            && self.exact.is_empty()
            && self.any_one.is_none()
    }
}

fn add_all<K: Clone + PartialEq>(found: &mut Vec<K>, listeners: &[K]) {
    for listener in listeners {
        if !found.contains(listener) {
            found.push(listener.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str) -> Group {
        Group::new(name).expect("the test names a valid group")
    }

    fn pattern(name: &str) -> Pattern {
        Pattern::new(name).expect("the test names a valid pattern")
    }

    #[test]
    fn a_group_refuses_wildcards_and_stray_characters() {
        assert!(Group::new("quest.4uhc5q").is_ok(), "a plain name is taken");
        for bad in ["", "quest..x", "quest.*", "Quest", "quest/x", "quest.>"] {
            assert!(Group::new(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_pattern_allows_a_final_rest_only() {
        assert!(Pattern::new("quest.*.x").is_ok(), "`*` may sit anywhere");
        assert!(Pattern::new("quest.>").is_ok(), "`>` may end a pattern");
        assert!(
            Pattern::new("quest.>.x").is_err(),
            "`>` may not sit in the middle"
        );
    }

    #[test]
    fn a_pattern_matches_by_token() {
        let cases = [
            ("quest.4uhc5q", "quest.4uhc5q", true),
            ("quest.*", "quest.4uhc5q", true),
            ("quest.*", "quest.4uhc5q.x", false),
            ("quest.>", "quest.4uhc5q.x", true),
            ("quest.>", "quest", false),
            (">", "machine", true),
            ("peer.*", "quest.x", false),
        ];
        for (wanted, given, hears) in cases {
            assert_eq!(
                pattern(wanted).matches(&group(given)),
                hears,
                "{wanted} hearing {given}"
            );
        }
    }

    #[test]
    fn routes_find_every_listener_once() {
        let mut routes = Routes::default();
        routes.insert(&pattern("quest.4uhc5q"), 1);
        routes.insert(&pattern("quest.*"), 2);
        routes.insert(&pattern("quest.>"), 3);
        routes.insert(&pattern(">"), 4);
        routes.insert(&pattern("peer.*"), 5);
        routes.insert(&pattern("quest.*"), 1);

        let mut heard = routes.listeners([&group("quest.4uhc5q"), &group("quest.x")]);
        heard.sort_unstable();
        assert_eq!(heard, vec![1, 2, 3, 4]);
        assert_eq!(routes.listeners([&group("quest.4uhc5q.x")]), vec![4, 3]);
    }

    #[test]
    fn a_removed_listener_hears_nothing() {
        let mut routes = Routes::default();
        routes.insert(&pattern("quest.*"), 1);
        routes.insert(&pattern("quest.>"), 2);
        routes.remove(&1);
        assert_eq!(routes.listeners([&group("quest.x")]), vec![2]);
        routes.remove(&2);
        assert!(
            routes.root.is_empty(),
            "the trie should hold no empty branches"
        );
    }
}
