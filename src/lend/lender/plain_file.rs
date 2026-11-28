//! Reading the files a lend turns on, its lend, its cuts, the day's tally
//! and spend-watch's word, so what stands in a file's place can't hold
//! the reader: a pipe opened to read waits for a writer, and the lend is
//! read at every ask and every `health`.

use std::fs;
use std::io::{self, Read as _};
use std::path::Path;

use rustix::fs::{Mode, OFlags};

/// What the plain file `file` holds, past any links. It is opened never
/// waiting on a pipe's writer, then judged by what was opened, so nothing
/// put in its place between judging and reading is read. Anything but a
/// plain file is refused as [`io::ErrorKind::InvalidData`]; one that isn't
/// there is [`io::ErrorKind::NotFound`].
pub(super) fn read(file: &Path) -> io::Result<Vec<u8>> {
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    let mut opened = fs::File::from(rustix::fs::open(file, flags, Mode::empty())?);
    if !opened.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "it isn't a plain file",
        ));
    }
    let mut text = Vec::new();
    opened.read_to_end(&mut text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// Longer than reading a small file should take, so a test fails
    /// rather than hangs.
    const PATIENCE: Duration = Duration::from_secs(10);

    #[test]
    fn a_plain_file_is_read_whole() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("cuts.json");
        fs::write(&file, "[\"agents-one\"]").expect("the cuts are written");
        let text = read(&file).expect("a plain file is read");
        assert_eq!(text, b"[\"agents-one\"]");
    }

    #[test]
    fn a_file_that_isnt_there_is_not_found() {
        let folder = tempfile::tempdir().expect("a folder");
        let error = read(&folder.path().join("cuts.json")).expect_err("nothing is there");
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    }

    #[test]
    fn a_pipe_in_a_files_place_is_refused_rather_than_waited_on() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("cuts.json");
        let made = Command::new("mkfifo")
            .arg(&file)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo made no pipe: {made}");
        // Read apart, so a pipe waited on fails the test rather than
        // hanging it.
        let (said, heard) = mpsc::channel();
        thread::spawn(move || said.send(read(&file).map_err(|error| error.kind())));
        let read = heard
            .recv_timeout(PATIENCE)
            .expect("a pipe is never waited on");
        assert_eq!(read, Err(io::ErrorKind::InvalidData), "a pipe is refused");
    }

    #[test]
    fn a_folder_in_a_files_place_is_refused() {
        let folder = tempfile::tempdir().expect("a folder");
        let error = read(folder.path()).expect_err("a folder isn't read");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
    }
}
