//! A client of a node's socket: one request and its reply, or a watch's
//! events as they come.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use crate::wire::{Event, Reply, Request};

/// A connection to a node.
pub struct Client {
    reading: BufReader<UnixStream>,
    writing: UnixStream,
}

impl Client {
    /// Connects to the node serving `socket`.
    pub fn connect(socket: &Path) -> io::Result<Self> {
        let writing = UnixStream::connect(socket).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("no node answers at {}: {error}", socket.display()),
            )
        })?;
        let reading = BufReader::new(writing.try_clone()?);
        Ok(Self { reading, writing })
    }

    /// Sends `request` and reads the node's reply.
    pub fn ask(&mut self, request: &Request) -> io::Result<Reply> {
        let mut line = serde_json::to_vec(request).map_err(io::Error::other)?;
        line.push(b'\n');
        self.writing.write_all(&line)?;
        self.read()
    }

    /// The next event of a watch, or nothing once the node has gone.
    pub fn next_event(&mut self) -> io::Result<Option<Event>> {
        let mut line = String::new();
        if self.reading.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        serde_json::from_str(&line)
            .map(Some)
            .map_err(io::Error::other)
    }

    fn read<T: for<'de> serde::Deserialize<'de>>(&mut self) -> io::Result<T> {
        let mut line = String::new();
        if self.reading.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the node hung up",
            ));
        }
        serde_json::from_str(&line).map_err(io::Error::other)
    }
}
