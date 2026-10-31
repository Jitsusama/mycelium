//! What another machine's link key may run on this one. Its key is pinned
//! in this account's `authorized_keys` to `mycelium --realm REALM ssh
//! --from NAME`, so whatever its SSH client asks, this runs, and reads
//! what was asked from `SSH_ORIGINAL_COMMAND`. Only the two things a node
//! asks of another machine are let through: its end of a link, and
//! joining a session held here. A link run so knows the machine at its
//! far end is NAME, proven by its key, and refuses an end saying it is
//! another.

/// What a pinned key asked, of what it may.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// This machine's end of a link, in the realm.
    Link,
    /// Joining the session held here by this id.
    Attach(String),
}

/// What `original`, the command a pinned key's client asked for, asks of
/// those it may in `realm`, or why it is refused.
///
/// A node asks `mycelium link --realm REALM` for a link and `mycelium
/// --realm REALM attach ID` to join a session, each word plain, since
/// either is all that a node dials with; any other words are refused,
/// whatever program the first names.
///
/// # Errors
///
/// Anything else, including no command at all, which is a login.
pub fn asked(original: Option<&str>, realm: &str) -> Result<Asked, String> {
    let Some(original) = original else {
        return Err("a link key logs in to no shell".to_owned());
    };
    let words: Vec<&str> = original.split_ascii_whitespace().collect();
    let plain = |word: &&str| {
        word.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&byte))
    };
    if !words.iter().all(plain) {
        return Err(format!(
            "{original:?} isn't plain words, which a node asks in"
        ));
    }
    let realm_is = |asked: &str| {
        if asked == realm {
            Ok(())
        } else {
            Err(format!(
                "a link key of realm {realm} can't ask in realm {asked}"
            ))
        }
    };
    match words.as_slice() {
        [program, "link", "--realm", asked] if is_mycelium(program) => {
            realm_is(asked).map(|()| Asked::Link)
        }
        [program, "--realm", asked, "attach", id] if is_mycelium(program) => {
            realm_is(asked).map(|()| Asked::Attach((*id).to_owned()))
        }
        _ => Err(format!(
            "{original:?} isn't a link or an attach, all a link key may ask"
        )),
    }
}

/// Whether `program` names the `mycelium` command, by any path.
fn is_mycelium(program: &str) -> bool {
    std::path::Path::new(program)
        .file_name()
        .is_some_and(|name| name == "mycelium")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_or_an_attach_in_its_realm_is_let_through() {
        assert_eq!(
            asked(Some("mycelium link --realm work"), "work"),
            Ok(Asked::Link),
            "a node's end of a link"
        );
        assert_eq!(
            asked(
                Some("/etc/profiles/per-user/joel/bin/mycelium link --realm work"),
                "work"
            ),
            Ok(Asked::Link),
            "by whatever path the dialler names it"
        );
        assert_eq!(
            asked(Some("mycelium --realm work attach 7bf12940"), "work"),
            Ok(Asked::Attach("7bf12940".to_owned())),
            "joining a held session"
        );
    }

    #[test]
    fn anything_else_is_refused_saying_why() {
        let refused = |original: Option<&str>| asked(original, "work").err();
        assert_eq!(
            refused(None),
            Some("a link key logs in to no shell".to_owned()),
            "no command is a login"
        );
        assert_eq!(
            refused(Some("mycelium link --realm personal")),
            Some("a link key of realm work can't ask in realm personal".to_owned()),
            "another realm's link"
        );
        for other in [
            "sh -c id",
            "mycelium --realm work node",
            "mycelium link --realm work --state /tmp/elsewhere",
            "evil/mycelium-not link --realm work",
        ] {
            assert!(
                refused(Some(other)).is_some_and(|why| why.contains("isn't a link or an attach")),
                "{other:?} is refused"
            );
        }
        assert!(
            refused(Some("mycelium --realm work attach 'x; id'"))
                .is_some_and(|why| why.contains("isn't plain words")),
            "a quoted word is refused before it is read"
        );
    }
}
