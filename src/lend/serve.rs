//! The lend's end on a machine borrowing it: pi's model requests, taken
//! as HTTP/1.1 on a Unix socket in the shape pi speaks to a machine's
//! model relay, each handed to this machine's node as a lend and its
//! answer streamed back to pi as the lender sends it.
//!
//! Nothing here holds a key or a token, and none is asked for: the node
//! carries each lend over the link it already holds to the lender, which
//! knows this machine by that link alone. Only the headers a model's
//! answer turns on are handed on, and the lender filters them again. A
//! refusal before any answer began is said to pi as its status, with an
//! error shaped as Anthropic's are; one after it began ends the
//! connection mid-answer, so pi sees the stream fail rather than end.
//! A client that hangs up hangs this up on the node, which lets the
//! lend go.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::UnixStream;

use crate::lend::{Lend, Lent};
use crate::node::Socket;
use crate::wire::Request;

/// The most a request's line and headers may hold, together.
pub const MOST_HEAD: usize = 64 * 1024;

/// The most a request's body may hold: a long conversation sent whole,
/// with room to spare.
pub const MOST_BODY: usize = 32 * 1024 * 1024;

/// The only request headers handed on: those a model's answer turns on.
/// Every other one, pi's own credentials above all, stays here, and the
/// lender carries on these alone too.
pub const HANDED_ON: [&str; 4] = [
    "content-type",
    "anthropic-version",
    "anthropic-beta",
    "accept",
];

/// The answer's headers that speak of one connection rather than the
/// answer, which this end says for itself.
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
];

/// Serves pi's requests on `socket` until it fails to accept, handing
/// each to the node at `node`, as lent from the budget `budget` names
/// when it names one.
///
/// # Errors
///
/// When the socket stops accepting.
pub async fn serve(socket: Socket, node: PathBuf, budget: Option<String>) -> io::Result<()> {
    loop {
        let stream = socket.accept().await?;
        tokio::spawn(connection(stream, node.clone(), budget.clone()));
    }
}

/// Answers one connection's requests in turn, for as long as its client
/// keeps it and each answer ended whole.
async fn connection(stream: UnixStream, node: PathBuf, budget: Option<String>) {
    let (reading, mut writing) = stream.into_split();
    let mut reading = BufReader::new(reading);
    loop {
        let asked = match read(&mut reading).await {
            Ok(Some(asked)) => asked,
            Ok(None) => return,
            Err(refused) => {
                // A client that left hears nothing, and one whose request
                // couldn't be read can't be read further.
                let _ = refuse(&mut writing, refused.status, &refused.why).await;
                return;
            }
        };
        let keeps = asked.keeps;
        let whole = if let Some(coding) = asked.coding.as_deref() {
            // pi's zstd layer sends plain again on a 415, so the lender only
            // ever sees a body as it was written.
            refuse(
                &mut writing,
                415,
                &format!("a lend carries no body coded as {coding}; send it plain"),
            )
            .await
            .is_ok()
        } else {
            let lend = Lend {
                budget: budget.clone(),
                method: asked.method,
                path: asked.path,
                headers: asked.headers,
                body: STANDARD.encode(&asked.body),
            };
            lent(&node, lend, asked.bodiless, &mut reading, &mut writing).await
        };
        if !(whole && keeps) {
            return;
        }
    }
}

/// A request read from pi, with what of it is handed on.
#[derive(Debug, PartialEq, Eq)]
struct Asked {
    method: String,
    path: String,
    /// Only those of [`HANDED_ON`], in the order pi sent them.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// How its body is coded, where pi says it is coded at all.
    coding: Option<String>,
    /// Whether the connection may carry another request after this one.
    keeps: bool,
    /// Whether its answer carries no body, as a HEAD's never does.
    bodiless: bool,
}

/// Why a request can't be lent, as the status pi hears.
#[derive(Debug, PartialEq, Eq)]
struct Refused {
    status: u16,
    why: String,
}

fn bad(why: impl Into<String>) -> Refused {
    Refused {
        status: 400,
        why: why.into(),
    }
}

/// Reads the next request on a connection, or none when its client hung
/// up between requests.
async fn read<R: AsyncBufRead + Unpin>(reading: &mut R) -> Result<Option<Asked>, Refused> {
    let mut spent = 0;
    let Some(first) = line(reading, &mut spent).await? else {
        return Ok(None);
    };
    let mut words = first.split(' ');
    let (Some(method), Some(path), Some(version), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return Err(bad("the request line isn't one"));
    };
    if !path.starts_with('/') {
        return Err(bad("a lend asks a path, never another place"));
    }
    let old = match version {
        "HTTP/1.1" => false,
        "HTTP/1.0" => true,
        _ => {
            return Err(Refused {
                status: 505,
                why: format!("{version} isn't spoken here; HTTP/1.1 is"),
            });
        }
    };
    let (mut length, mut chunked, mut close, mut keep_alive, mut coding) =
        (None, false, false, false, None);
    let mut headers = Vec::new();
    loop {
        let Some(header) = line(reading, &mut spent).await? else {
            return Err(bad("the request ended within its headers"));
        };
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| bad("a header has no colon"))?;
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        match name.as_str() {
            "content-length" => {
                let said = value
                    .parse::<usize>()
                    .map_err(|_| bad("the content length isn't a number"))?;
                if length.is_some_and(|length| length != said) {
                    return Err(bad("two content lengths disagree"));
                }
                length = Some(said);
            }
            "transfer-encoding" => {
                if !value.eq_ignore_ascii_case("chunked") {
                    return Err(Refused {
                        status: 501,
                        why: format!("a body sent as {value} isn't read here"),
                    });
                }
                chunked = true;
            }
            "connection" => {
                for token in value.split(',').map(str::trim) {
                    close |= token.eq_ignore_ascii_case("close");
                    keep_alive |= token.eq_ignore_ascii_case("keep-alive");
                }
            }
            "content-encoding" if !value.eq_ignore_ascii_case("identity") => {
                coding = Some(value.to_owned());
            }
            _ if HANDED_ON.contains(&name.as_str()) => headers.push((name, value.to_owned())),
            _ => {}
        }
    }
    if chunked && length.is_some() {
        return Err(bad("a body can't be both chunked and of a length"));
    }
    let body = if chunked {
        chunks(reading).await?
    } else {
        let length = length.unwrap_or(0);
        if length > MOST_BODY {
            return Err(too_long());
        }
        let mut body = vec![0; length];
        reading
            .read_exact(&mut body)
            .await
            .map_err(|_| bad("the request ended within its body"))?;
        body
    };
    Ok(Some(Asked {
        bodiless: method == "HEAD",
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        body,
        coding,
        keeps: if old { keep_alive } else { !close },
    }))
}

fn too_long() -> Refused {
    Refused {
        status: 413,
        why: format!("a lend's body is at most {MOST_BODY} bytes"),
    }
}

/// The next line of a request's head, without its line end, counting
/// what it took against [`MOST_HEAD`]; none when the client hung up
/// before saying any of it.
async fn line<R: AsyncBufRead + Unpin>(
    reading: &mut R,
    spent: &mut usize,
) -> Result<Option<String>, Refused> {
    let mut said = Vec::new();
    let room = u64::try_from(MOST_HEAD - *spent).unwrap_or(u64::MAX);
    let read = (&mut *reading)
        .take(room)
        .read_until(b'\n', &mut said)
        .await
        .map_err(|_| bad("the request couldn't be read"))?;
    *spent += read;
    if read == 0 && *spent == 0 {
        return Ok(None);
    }
    let Some(said) = said.strip_suffix(b"\n") else {
        return Err(if *spent >= MOST_HEAD {
            Refused {
                status: 431,
                why: format!("a request's head is at most {MOST_HEAD} bytes"),
            }
        } else {
            bad("the request ended within its head")
        });
    };
    let said = said.strip_suffix(b"\r").unwrap_or(said);
    String::from_utf8(said.to_vec())
        .map(Some)
        .map_err(|_| bad("a request's head is text"))
}

/// A body sent in chunks, read whole.
async fn chunks<R: AsyncBufRead + Unpin>(reading: &mut R) -> Result<Vec<u8>, Refused> {
    let mut body = Vec::new();
    loop {
        let mut spent = 0;
        let size = line(reading, &mut spent)
            .await?
            .ok_or_else(|| bad("the request ended within its body"))?;
        let size = size.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16).map_err(|_| bad("a chunk's size isn't one"))?;
        if size == 0 {
            // Trailers, which hand nothing on, up to the blank line.
            loop {
                let mut spent = 0;
                match line(reading, &mut spent).await? {
                    Some(trailer) if !trailer.is_empty() => {}
                    _ => return Ok(body),
                }
            }
        }
        if body.len() + size > MOST_BODY {
            return Err(too_long());
        }
        let start = body.len();
        body.resize(start + size, 0);
        reading
            .read_exact(&mut body[start..])
            .await
            .map_err(|_| bad("the request ended within its body"))?;
        let mut end = [0; 2];
        reading
            .read_exact(&mut end)
            .await
            .map_err(|_| bad("the request ended within its body"))?;
        if end != *b"\r\n" {
            return Err(bad("a chunk runs past its size"));
        }
    }
}

/// Hands `lend` to the node at `node` and streams its answer to pi as it
/// comes, answering whether it ended whole, so the connection may carry
/// another request. Where pi hangs up meanwhile, the node is hung up on,
/// which lets the lend go.
async fn lent<R, W>(
    node: &Path,
    lend: Lend,
    bodiless: bool,
    reading: &mut R,
    writing: &mut W,
) -> bool
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let asked = match ask(node, lend).await {
        Ok(asked) => asked,
        Err(why) => return refuse(writing, 503, &why).await.is_ok(),
    };
    let mut lines = asked.lines();
    let mut began = false;
    // Whether pi might still hang up, which stops being watched once it
    // says more, as a pipelining client would.
    let mut watching = true;
    loop {
        let said = tokio::select! {
            said = lines.next_line() => said,
            gone = reading.fill_buf(), if watching => {
                match gone {
                    Ok(more) if !more.is_empty() => watching = false,
                    // Dropping the node's connection lets the lend go.
                    _ => return false,
                }
                continue;
            }
        };
        let heard = match said {
            Ok(Some(line)) => serde_json::from_str::<Lent>(&line).map_err(|error| {
                format!("this machine's node said what isn't a lend's answer: {error}")
            }),
            _ => Err("this machine's node stopped answering".to_owned()),
        };
        let written = match (heard, began) {
            (Ok(Lent::Head { status, headers }), false) => {
                began = true;
                head(writing, status, &headers, bodiless).await
            }
            (Ok(Lent::Body(_)), true) if bodiless => Ok(()),
            (Ok(Lent::Body(piece)), true) => match STANDARD.decode(piece) {
                Ok(piece) if piece.is_empty() => Ok(()),
                Ok(piece) => chunk(writing, &piece).await,
                // Cut short, as any failure once the answer began.
                Err(_) => return false,
            },
            (Ok(Lent::Done), true) => {
                return if bodiless {
                    writing.flush().await.is_ok()
                } else {
                    writing.write_all(b"0\r\n\r\n").await.is_ok() && writing.flush().await.is_ok()
                };
            }
            (Ok(Lent::Refused { status, why }), false) => {
                return refuse(writing, status, &why).await.is_ok();
            }
            (Err(why), false) => return refuse(writing, 502, &why).await.is_ok(),
            (Ok(Lent::Done | Lent::Body(_)), false) => {
                return refuse(writing, 502, "the lender answered before its head")
                    .await
                    .is_ok();
            }
            // Once an answer began its status is said, so a failure ends
            // the connection short of the answer's end, which pi reads as
            // the answer failing.
            _ => return false,
        };
        if written.is_err() {
            return false;
        }
    }
}

/// The node's connection, `lend` asked of it.
async fn ask(node: &Path, lend: Lend) -> Result<BufReader<UnixStream>, String> {
    let mut stream = UnixStream::connect(node)
        .await
        .map_err(|error| format!("this machine's node didn't answer: {error}"))?;
    let mut line =
        serde_json::to_string(&Request::Lend(lend)).map_err(|error| error.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|error| format!("this machine's node didn't answer: {error}"))?;
    Ok(BufReader::new(stream))
}

/// Begins an answer of `status` with the lender's `headers`, but those of
/// one connection, its body to come in chunks unless it has none.
async fn head<W: AsyncWrite + Unpin>(
    writing: &mut W,
    status: u16,
    headers: &[(String, String)],
    bodiless: bool,
) -> io::Result<()> {
    let mut said = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        // A header that would break the head's lines isn't handed on.
        let whole = |text: &str| !text.contains(['\r', '\n']);
        if !HOP_BY_HOP.contains(&lower.as_str()) && whole(name) && whole(value) {
            // Writing to a String can't fail.
            let _ = write!(said, "{name}: {value}\r\n");
        }
    }
    if bodiless || matches!(status, 100..=199 | 204 | 304) {
        said.push_str("content-length: 0\r\n\r\n");
    } else {
        said.push_str("transfer-encoding: chunked\r\n\r\n");
    }
    writing.write_all(said.as_bytes()).await?;
    writing.flush().await
}

/// One piece of an answer's body, sent as it came.
async fn chunk<W: AsyncWrite + Unpin>(writing: &mut W, piece: &[u8]) -> io::Result<()> {
    writing
        .write_all(format!("{:x}\r\n", piece.len()).as_bytes())
        .await?;
    writing.write_all(piece).await?;
    writing.write_all(b"\r\n").await?;
    writing.flush().await
}

/// Answers `status` with `why`, as an error shaped as Anthropic's are, so
/// pi's model client says it as it says the proxy's.
async fn refuse<W: AsyncWrite + Unpin>(writing: &mut W, status: u16, why: &str) -> io::Result<()> {
    let body = json!({
        "type": "error",
        "error": {"type": "mycelium_lend", "message": why},
    })
    .to_string();
    let said = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        reason(status),
        body.len()
    );
    writing.write_all(said.as_bytes()).await?;
    writing.flush().await
}

/// The words a status line carries, which clients don't read.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Lent",
    }
}

#[cfg(test)]
mod tests;
