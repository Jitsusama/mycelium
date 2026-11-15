//! The window title a held program last set, for a terminal joining
//! later. A program such as pi sets its title once, as it starts, and
//! often before any terminal has joined its holder, so a window that
//! joins after was never told it: a reopened session's window then went
//! untitled, and anything finding it by its title never did (core.nix's
//! session restores, 2026-11-15). A title isn't a mode, so it is kept
//! apart from [`super::modes`], and given after them.

/// The escape that opens a sequence.
const ESCAPE: u8 = 0x1b;

/// What ends an operating system command: the bell, or the string
/// terminator, `ESC \`.
const BELL: u8 = 0x07;

/// The longest title sequence kept while waiting for its end; one longer
/// is let go rather than held without bound.
const LONGEST: usize = 4096;

/// The last title a program set, as it set it.
#[derive(Debug, Default)]
pub struct Title {
    /// The whole sequence that set it, given again as it came.
    set: Option<Vec<u8>>,
    /// The start of a title sequence the last output ended inside.
    partial: Vec<u8>,
}

impl Title {
    /// Reads what the program wrote, `output`, for a title it sets. A
    /// sequence split between two writes is read once both have come.
    pub fn saw(&mut self, output: &[u8]) {
        let mut bytes = std::mem::take(&mut self.partial);
        bytes.extend_from_slice(output);
        let mut at = 0;
        while let Some(found) = bytes[at..].iter().position(|&byte| byte == ESCAPE) {
            let start = at + found;
            match command(&bytes[start..]) {
                Command::Title { text, end } => {
                    self.set = (!text.is_empty()).then(|| bytes[start..start + end].to_vec());
                    at = start + end;
                }
                Command::Other { end } => at = start + end,
                Command::Unfinished => {
                    if bytes.len() - start <= LONGEST {
                        self.partial = bytes[start..].to_vec();
                    }
                    return;
                }
            }
        }
    }

    /// What gives a fresh terminal the title set last, if there is one.
    pub fn restored(&self) -> Vec<u8> {
        self.set.clone().unwrap_or_default()
    }
}

/// What starts at an escape.
enum Command<'a> {
    /// A title set, as `OSC 0` or `OSC 2` does, its text, and the length
    /// the sequence ran to.
    Title { text: &'a [u8], end: usize },
    /// Anything else, and how far to read on from: past a whole command
    /// that sets no title, or past the escape alone.
    Other { end: usize },
    /// The start of an operating system command whose end hasn't come.
    Unfinished,
}

/// Reads the operating system command `bytes` starts with: an escape,
/// `]`, its number, `;`, its text, then the bell or `ESC \`. An escape
/// inside it that starts anything else ends it unread, as terminals do.
fn command(bytes: &[u8]) -> Command<'_> {
    match bytes.get(1) {
        None => return Command::Unfinished,
        Some(b']') => {}
        Some(_) => return Command::Other { end: 1 },
    }
    let mut at = 2;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    let number = &bytes[2..at];
    match bytes.get(at) {
        None => return Command::Unfinished,
        Some(b';') => {}
        Some(_) => return Command::Other { end: 1 },
    }
    let text = at + 1;
    for (found, &byte) in bytes.iter().enumerate().skip(text) {
        let end = match (byte, bytes.get(found + 1)) {
            (BELL, _) => found + 1,
            (ESCAPE, Some(b'\\')) => found + 2,
            (ESCAPE, Some(_)) => return Command::Other { end: found },
            (ESCAPE, None) => return Command::Unfinished,
            _ => continue,
        };
        return if number == b"0" || number == b"2" {
            Command::Title {
                text: &bytes[text..found],
                end,
            }
        } else {
            Command::Other { end }
        };
    }
    Command::Unfinished
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a fresh terminal is given once a program wrote `output`, in
    /// writes of `each` bytes.
    fn restored_after(output: &str, each: usize) -> String {
        let mut title = Title::default();
        for part in output.as_bytes().chunks(each) {
            title.saw(part);
        }
        String::from_utf8(title.restored()).expect("a title is text")
    }

    #[test]
    fn the_title_a_program_set_is_given_back_as_it_set_it() {
        assert_eq!(
            restored_after("\x1b[?1049hhello\x1b]2;pi --session 01c8-far\x07drawn", 64),
            "\x1b]2;pi --session 01c8-far\x07",
        );
    }

    #[test]
    fn only_the_last_title_is_given() {
        assert_eq!(
            restored_after("\x1b]0;first\x07 then \x1b]2;second\x1b\\ more", 64),
            "\x1b]2;second\x1b\\",
            "either ending, and the latest wins"
        );
    }

    #[test]
    fn a_title_split_between_writes_is_still_read() {
        let output = "plain \x1b]2;pi --session 01c5-under\x07 drawn";
        for each in 1..8 {
            assert_eq!(
                restored_after(output, each),
                "\x1b]2;pi --session 01c5-under\x07",
                "read {each} bytes at a time"
            );
        }
    }

    #[test]
    fn an_empty_title_clears_the_one_before() {
        assert_eq!(restored_after("\x1b]2;pi\x07\x1b]2;\x07", 3), "");
    }

    #[test]
    fn an_icon_name_and_other_commands_are_no_title() {
        assert_eq!(
            restored_after(
                "\x1b]1;icon\x07\x1b]8;;https://example.org\x07link\x1b]8;;\x07\x1b[1mbold",
                5
            ),
            "",
            "an icon name, a link and a colour set no title"
        );
    }

    #[test]
    fn a_title_that_never_ends_is_let_go() {
        let endless = format!("\x1b]2;{}", "x".repeat(LONGEST * 2));
        assert_eq!(restored_after(&endless, 512), "");
        let mut title = Title::default();
        title.saw(endless.as_bytes());
        title.saw(b"\x1b]2;after\x07");
        assert_eq!(
            title.restored(),
            b"\x1b]2;after\x07",
            "and one after it is still read"
        );
    }
}
