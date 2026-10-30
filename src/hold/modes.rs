//! What a held program has asked of its terminal, beyond what it draws:
//! its private modes, such as the alternate screen, mouse reports and
//! bracketed paste, and the keys it asked to be told of (kitty's keyboard
//! flags, xterm's modifyOtherKeys). A program asks once, as it starts, so
//! a terminal joining later is given them before anything else, or it
//! would draw a full-screen program on its main screen and keep its wheel
//! and its pastes to itself.

use std::collections::BTreeMap;

/// The escape that opens a control sequence.
const ESCAPE: u8 = 0x1b;

/// The longest control sequence kept while waiting for its end; one
/// longer is no mode, and is let go.
const LONGEST: usize = 64;

/// Private modes saying which screen is drawn on. They are given last, so
/// what the main screen was asked comes before the switch away from it.
const SCREENS: [u16; 3] = [47, 1047, 1049];

/// Synchronized output brackets a frame rather than setting a mode, and
/// the frame's end comes with the output anyway.
const FRAME: u16 = 2026;

/// What a program has asked of its terminal so far.
#[derive(Debug, Default)]
pub struct Modes {
    /// Each private mode by its number, and whether it is set.
    private: BTreeMap<u16, bool>,
    /// Kitty's keyboard flags, a stack for each screen: the main one's,
    /// then the alternate one's.
    keys: [Vec<u32>; 2],
    /// xterm's modifyOtherKeys level, once one is asked for.
    other_keys: Option<u32>,
    /// The start of a control sequence the last output ended inside.
    partial: Vec<u8>,
}

impl Modes {
    /// Reads what the program wrote, `output`, for the modes it asks for.
    /// A sequence split between two writes is read once both have come.
    pub fn saw(&mut self, output: &[u8]) {
        let mut bytes = std::mem::take(&mut self.partial);
        bytes.extend_from_slice(output);
        let mut at = 0;
        while let Some(found) = bytes[at..].iter().position(|&byte| byte == ESCAPE) {
            let start = at + found;
            match sequence(&bytes[start..]) {
                Sequence::Whole { asked, end } => {
                    self.asked(asked);
                    at = start + end;
                }
                Sequence::Unfinished => {
                    if bytes.len() - start <= LONGEST {
                        self.partial = bytes[start..].to_vec();
                    }
                    return;
                }
                Sequence::Other => at = start + 1,
            }
        }
    }

    /// What gives a fresh terminal the modes asked for so far, to be
    /// written to it before the program's output.
    pub fn restored(&self) -> Vec<u8> {
        let mut restored = Vec::new();
        let set = |restored: &mut Vec<u8>, mode: u16, on: bool| {
            let end = if on { 'h' } else { 'l' };
            restored.extend(format!("\x1b[?{mode}{end}").bytes());
        };
        if let Some(&flags) = self.keys[0].last() {
            restored.extend(format!("\x1b[>{flags}u").bytes());
        }
        for (&mode, &on) in &self.private {
            if !SCREENS.contains(&mode) {
                set(&mut restored, mode, on);
            }
        }
        for (&mode, &on) in &self.private {
            if SCREENS.contains(&mode) {
                set(&mut restored, mode, on);
            }
        }
        if let Some(&flags) = self.keys[1].last() {
            restored.extend(format!("\x1b[>{flags}u").bytes());
        }
        if let Some(level) = self.other_keys {
            restored.extend(format!("\x1b[>4;{level}m").bytes());
        }
        restored
    }

    /// Whether the program draws on the alternate screen now.
    fn alternate(&self) -> bool {
        SCREENS
            .iter()
            .any(|mode| self.private.get(mode) == Some(&true))
    }

    /// Takes in one control sequence the program wrote.
    fn asked(&mut self, asked: Asked<'_>) {
        let screen = usize::from(self.alternate());
        match asked {
            Asked::Private { modes, on } => {
                for mode in numbers(modes).filter(|&mode| mode != u32::from(FRAME)) {
                    if let Ok(mode) = u16::try_from(mode) {
                        self.private.insert(mode, on);
                    }
                }
            }
            Asked::PushKeys(flags) => self.keys[screen].push(flags.parse().unwrap_or(0)),
            Asked::PopKeys(count) => {
                let count = count.parse().unwrap_or(1);
                let keys = &mut self.keys[screen];
                keys.truncate(keys.len().saturating_sub(count));
            }
            Asked::SetKeys(said) => {
                let mut said = numbers(said);
                let flags = said.next().unwrap_or(0);
                let keys = &mut self.keys[screen];
                let was = keys.pop().unwrap_or(0);
                keys.push(match said.next().unwrap_or(1) {
                    2 => was | flags,
                    3 => was & !flags,
                    _ => flags,
                });
            }
            Asked::OtherKeys(said) => {
                let mut said = numbers(said);
                if said.next() == Some(4) {
                    self.other_keys = Some(said.next().unwrap_or(0));
                }
            }
        }
    }
}

/// The numbers in a sequence's parameters, `1;2;3`.
fn numbers(said: &str) -> impl Iterator<Item = u32> + '_ {
    said.split(';').filter_map(|number| number.parse().ok())
}

/// A control sequence that sets one of the modes kept.
#[derive(Clone, Copy)]
enum Asked<'a> {
    /// `CSI ? modes h` sets each private mode, and `l` resets it.
    Private { modes: &'a str, on: bool },
    /// `CSI > flags u` pushes keyboard flags.
    PushKeys(&'a str),
    /// `CSI < count u` pops them.
    PopKeys(&'a str),
    /// `CSI = flags ; how u` sets, adds or takes away flags in place.
    SetKeys(&'a str),
    /// `CSI > 4 ; level m` sets modifyOtherKeys.
    OtherKeys(&'a str),
}

/// What starts at an escape.
enum Sequence<'a> {
    /// A sequence setting a mode kept, and the length it ran to.
    Whole { asked: Asked<'a>, end: usize },
    /// The start of a control sequence whose end hasn't come yet.
    Unfinished,
    /// Anything else, which says nothing of modes.
    Other,
}

/// Reads the control sequence `bytes` starts with: an escape, `[`, its
/// parameters and intermediates, then one final byte.
fn sequence(bytes: &[u8]) -> Sequence<'_> {
    match bytes.get(1) {
        None => return Sequence::Unfinished,
        Some(b'[') => {}
        Some(_) => return Sequence::Other,
    }
    let Some(last) = bytes[2..]
        .iter()
        .position(|&byte| !(0x20..=0x3f).contains(&byte))
        .map(|found| found + 2)
    else {
        return Sequence::Unfinished;
    };
    let (Ok(said), end) = (std::str::from_utf8(&bytes[2..last]), last + 1) else {
        return Sequence::Other;
    };
    let asked = match (said.as_bytes().first(), bytes[last]) {
        (Some(b'?'), b'h') => Asked::Private {
            modes: &said[1..],
            on: true,
        },
        (Some(b'?'), b'l') => Asked::Private {
            modes: &said[1..],
            on: false,
        },
        (Some(b'>'), b'u') => Asked::PushKeys(&said[1..]),
        (Some(b'<'), b'u') => Asked::PopKeys(&said[1..]),
        (Some(b'='), b'u') => Asked::SetKeys(&said[1..]),
        (Some(b'>'), b'm') => Asked::OtherKeys(&said[1..]),
        _ => return Sequence::Other,
    };
    Sequence::Whole { asked, end }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a fresh terminal is given once a program wrote `output`, in
    /// writes of `each` bytes.
    fn restored_after(output: &str, each: usize) -> String {
        let mut modes = Modes::default();
        for part in output.as_bytes().chunks(each) {
            modes.saw(part);
        }
        String::from_utf8(modes.restored()).expect("modes are text")
    }

    /// How a full-screen program such as pi starts: its keys first, then
    /// the alternate screen and the mouse, among what it draws.
    const FULL_SCREEN: &str = "\x1b[>7u\x1b[?u\x1b[?2004hhello\x1b[?1049h\x1b[?7l\
        \x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h\x1b[2J\x1b[H\x1b[?25l\
        \x1b[?2026hdrawn\x1b[?2026l";

    #[test]
    fn a_full_screen_programs_modes_are_given_back_screen_last() {
        assert_eq!(
            restored_after(FULL_SCREEN, FULL_SCREEN.len()),
            "\x1b[>7u\x1b[?7l\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h\
             \x1b[?2004h\x1b[?1049h",
            "its keys on the main screen, its modes, then its screen, and no frame"
        );
    }

    #[test]
    fn a_mode_split_between_writes_is_still_read() {
        for each in 1..8 {
            assert_eq!(
                restored_after(FULL_SCREEN, each),
                restored_after(FULL_SCREEN, FULL_SCREEN.len()),
                "read {each} bytes at a time, the same modes"
            );
        }
    }

    #[test]
    fn what_a_program_takes_back_is_given_back_as_it_left_it() {
        assert_eq!(
            restored_after(
                "\x1b[?1049h\x1b[>1u\x1b[=2;2u\x1b[?1000;1006h\x1b[?1000l\
                 \x1b[<u\x1b[?1049l",
                5
            ),
            "\x1b[?1000l\x1b[?1006h\x1b[?1049l",
            "the mouse it turned off stays off, and the keys it popped are gone"
        );
    }

    #[test]
    fn keys_asked_for_on_the_alternate_screen_are_given_there() {
        assert_eq!(
            restored_after("\x1b[?1049h\x1b[>5u\x1b[=2;2u\x1b[>4;2m", 3),
            "\x1b[?1049h\x1b[>7u\x1b[>4;2m",
            "flags pushed then added to, after the switch"
        );
    }

    #[test]
    fn a_program_asking_nothing_is_given_nothing() {
        assert_eq!(
            restored_after(
                "plain \x1b[1mbold\x1b[0m \x1b]0;title\x07 \x1b[38;5;2mtext",
                4
            ),
            "",
            "colours and titles aren't modes"
        );
    }
}
