//! A terminal joining a holder: what it types and each size it takes go to
//! the holder, framed, and the program's output comes back as it came.

use std::io;
use std::path::Path;

use rustix::termios::{self, OptionalActions, Termios};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use super::{Said, Size};
use crate::record::Window;

/// The size a terminal that won't say its own is taken to have.
const SIZE_UNSAID: Size = Size { rows: 24, cols: 80 };

/// Joins the holder serving `socket` from this process's own terminal,
/// raw while joined, telling the holder the window it is and each size it
/// takes, until the program ends.
pub async fn join_here(socket: &Path) -> io::Result<()> {
    let window = window_of(|name| std::env::var(name).ok());
    let _raw = Raw::enter()?;
    let (tell, sizes) = mpsc::unbounded_channel();
    let mut changes = signal(SignalKind::window_change())?;
    let watching = async move {
        while changes.recv().await.is_some() {
            if let Some(size) = size_here()
                && tell.send(size).is_err()
            {
                return;
            }
        }
    };
    let size = size_here().unwrap_or(SIZE_UNSAID);
    tokio::select! {
        joined = join(socket, size, window, tokio::io::stdin(), tokio::io::stdout(), sizes) => joined,
        () = watching => Ok(()),
    }
}

/// The window a terminal is, from the variables `named` gives, as the
/// harness names it: a kitty window by its id within kitty's socket.
/// Other terminals don't say yet.
pub fn window_of(named: impl Fn(&str) -> Option<String>) -> Option<Window> {
    let id = named("KITTY_WINDOW_ID").filter(|id| !id.is_empty())?;
    Some(Window {
        terminal: "kitty".to_owned(),
        id,
        within: named("KITTY_LISTEN_ON").filter(|within| !within.is_empty()),
    })
}

/// This process's terminal's size, if it has one that says.
fn size_here() -> Option<Size> {
    let size = termios::tcgetwinsize(io::stdout()).ok()?;
    (size.ws_row > 0 && size.ws_col > 0).then_some(Size {
        rows: size.ws_row,
        cols: size.ws_col,
    })
}

/// This process's terminal in raw mode, so every key goes to the program
/// as typed, back as it was when dropped. Input that isn't a terminal is
/// left as it is.
struct Raw(Option<Termios>);

impl Raw {
    fn enter() -> io::Result<Self> {
        let input = io::stdin();
        if !termios::isatty(&input) {
            return Ok(Self(None));
        }
        let was = termios::tcgetattr(&input)?;
        let mut raw = was.clone();
        raw.make_raw();
        termios::tcsetattr(&input, OptionalActions::Now, &raw)?;
        Ok(Self(Some(was)))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(was) = &self.0 {
            // A terminal gone by now has nothing to give back.
            let _ = termios::tcsetattr(io::stdin(), OptionalActions::Now, was);
        }
    }
}

/// Joins the holder serving `socket` at `size`, as `window` if it is one
/// that says: what `input` gives is typed into the program, each size from
/// `sizes` is the terminal's new one, and the program's output goes to
/// `output`. It answers once the holder lets
/// it go, which it does when the program ends. Once `input` ends, nothing
/// more is typed, but the program's output still comes.
pub async fn join(
    socket: &Path,
    size: Size,
    window: Option<Window>,
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    sizes: mpsc::UnboundedReceiver<Size>,
) -> io::Result<()> {
    let stream = UnixStream::connect(socket).await?;
    let (mut from, mut to) = stream.into_split();
    to.write_all(&Said::Size(size).encode()).await?;
    if let Some(window) = window {
        to.write_all(&Said::Window(window).encode()).await?;
    }
    let showing = async {
        tokio::io::copy(&mut from, &mut output).await?;
        output.flush().await
    };
    tokio::select! {
        shown = showing => shown,
        typed = type_into(&mut to, input, sizes) => typed,
    }
}

/// Says what `input` gives and each size from `sizes` to the holder, for
/// as long as the connection lasts.
async fn type_into(
    to: &mut (impl AsyncWrite + Unpin),
    mut input: impl AsyncRead + Unpin,
    mut sizes: mpsc::UnboundedReceiver<Size>,
) -> io::Result<()> {
    let mut buffer = vec![0u8; 4096];
    let mut typed = true;
    let mut resized = true;
    loop {
        tokio::select! {
            read = input.read(&mut buffer), if typed => match read {
                Ok(0) | Err(_) => typed = false,
                Ok(read) => to.write_all(&Said::Typed(buffer[..read].to_vec()).encode()).await?,
            },
            size = sizes.recv(), if resized => match size {
                Some(size) => to.write_all(&Said::Size(size).encode()).await?,
                None => resized = false,
            },
            // Nothing more will be said, and the holder would take a
            // closed connection as leaving, so the connection stays.
            else => std::future::pending::<()>().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kitty_window_says_which_it_is() {
        let kitty = |name: &str| match name {
            "KITTY_WINDOW_ID" => Some("13".to_owned()),
            "KITTY_LISTEN_ON" => Some("unix:@kitty-6753".to_owned()),
            _ => None,
        };
        assert_eq!(
            window_of(kitty),
            Some(Window {
                terminal: "kitty".to_owned(),
                id: "13".to_owned(),
                within: Some("unix:@kitty-6753".to_owned()),
            }),
            "its id within kitty's socket, as the harness names it"
        );
        assert_eq!(window_of(|_| None), None, "another terminal says nothing");
    }
}
