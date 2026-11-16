//! Programs the tests stand in for, put in place whole.
//!
//! A test that writes a program and then runs it races every other test
//! thread: one that forks while the file is still open for writing hands
//! its child that descriptor, and until the child runs its own program,
//! running the written one fails with "Text file busy". So a child
//! process writes it, and no descriptor open for writing it ever lives in
//! the tests' own process.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Puts a program saying `text` at `path`, runnable, written by a child.
///
/// # Panics
///
/// When the child can't be started or doesn't put it in place, since a
/// test without its stand-in proves nothing.
pub fn program(path: &Path, text: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 -- \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("a shell puts the stand-in in place");
    child
        .stdin
        .take()
        .expect("the shell's input")
        .write_all(text.as_bytes())
        .unwrap_or_else(|error| {
            panic!("the stand-in {} is told the shell: {error}", path.display())
        });
    let put = child.wait().expect("the shell ends");
    assert!(put.success(), "the stand-in {} is in place", path.display());
}
