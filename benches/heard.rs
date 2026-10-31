//! How long a post takes from being written into the mesh's folders, as a
//! pi session writes one, to being heard by a watch on the node's socket:
//! the whole path a message takes on one machine, through the folder's
//! watch, the node's routes and the socket.
//!
//! A node runs over folders of its own in a temporary folder. Each watch
//! hears a group of its own, and posts go to each group in turn at a
//! steady rate, each from the next of the senders. A post carries when it
//! was written, so the watch hearing it says how long it took, and the
//! write itself is timed apart, since the disk's own stalls are the
//! machine's and not the node's. The first second's posts are left out,
//! as the node and the machine warm up. `--folder` puts the mesh's folders
//! somewhere else, such as `/dev/shm`, to measure without the disk.
//!
//! `--across` writes the posts into a second machine's folders instead,
//! linked with the node's by a link over a pipe in the same process: a
//! link's own part in a post's time, with no network or SSH in it.
//!
//! ```sh
//! nix develop -c cargo bench --bench heard -- --watches 100 --rate 200 --seconds 10
//! ```

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clap::Parser;
use mycelium::awake::Awake;
use mycelium::client::Client;
use mycelium::group::{Group, Pattern};
use mycelium::link::discovery::Listed;
use mycelium::link::membership::Realm;
use mycelium::link::refused::Refusals;
use mycelium::link::{
    self,
    dial::{Connection, Ssh},
};
use mycelium::mesh;
use mycelium::node;
use mycelium::store::Store;
use mycelium::store::folders::Folders;
use mycelium::wire::{Draft, Event, Request, Response};

/// How long posts not yet heard are waited for once the last is written,
/// before they are counted as lost.
const STRAGGLERS_WITHIN: Duration = Duration::from_secs(5);

/// What to measure.
#[derive(Parser)]
struct Asked {
    /// Watches on the socket, each hearing a group of its own.
    #[arg(long, default_value_t = 100)]
    watches: u32,
    /// Sessions the posts come from, each in turn.
    #[arg(long, default_value_t = 1000)]
    senders: u32,
    /// Posts written a second.
    #[arg(long, default_value_t = 200)]
    rate: u32,
    /// For how many seconds.
    #[arg(long, default_value_t = 10)]
    seconds: u32,
    /// Where to make the temporary folder the mesh's folders go in, if not
    /// the system's temporary folder.
    #[arg(long)]
    folder: Option<PathBuf>,
    /// Write the posts on a second machine's folders, linked with the
    /// node's.
    #[arg(long)]
    across: bool,
    /// Given by `cargo bench` to every bench, and taken for nothing.
    #[arg(long, hide = true)]
    bench: bool,
}

/// A post's index, and how long it took to be heard.
type Heard = (u32, Duration);

/// Something run on a thread of its own, and how to stop it.
type Running = (tokio::sync::oneshot::Sender<()>, JoinHandle<io::Result<()>>);

/// How much a link's pipe holds each way.
const PIPE: usize = 1 << 16;

fn main() -> Result<(), Box<dyn Error>> {
    let asked = Asked::parse();
    if asked.watches == 0 || asked.senders == 0 || asked.rate == 0 || asked.seconds == 0 {
        return Err("watches, senders, rate and seconds must each be at least 1".into());
    }
    let total = asked
        .rate
        .checked_mul(asked.seconds)
        .ok_or("that many posts is more than can be counted")?;
    let folder = match &asked.folder {
        Some(within) => tempfile::tempdir_in(within)?,
        None => tempfile::tempdir()?,
    };
    let store = Folders::open(&folder.path().join("mesh"), &folder.path().join("claims"))?;
    let socket = folder.path().join("node").join("socket");

    let (stop, running) = run_node(store.clone(), &socket)?;
    let (written_to, linking) = if asked.across {
        let near = Folders::open(
            &folder.path().join("near-mesh"),
            &folder.path().join("near-claims"),
        )?;
        (near.clone(), Some(run_link(near, store.clone())?))
    } else {
        (store.clone(), None)
    };
    let start = Instant::now();
    let (heard, hearing) = mpsc::channel();
    let watching = (0..asked.watches)
        .map(|watch| watch_group(&socket, watch, start, heard.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    drop(heard);

    let mut writes = post(&asked, &written_to, start, total)?;
    let (got, mut times) = gather(&hearing, total, asked.rate);

    if let Some((stop, running)) = linking {
        // The link may already have stopped, if it failed.
        let _ = stop.send(());
        running.join().map_err(|_| "the link panicked")??;
    }
    // The node may already have stopped, if it failed.
    let _ = stop.send(());
    running.join().map_err(|_| "the node panicked")??;
    for watch in watching {
        watch.join().map_err(|_| "a watch panicked")?;
    }
    times.sort_unstable();
    writes.sort_unstable();
    report(&asked, got, total, &times, &writes);
    if got < total {
        return Err(format!("{} posts were never heard", total - got).into());
    }
    Ok(())
}

/// Runs a node over `store`, serving `socket`, on a thread of its own until
/// the sender answered is sent to or dropped.
fn run_node(store: Folders, socket: &Path) -> io::Result<Running> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // Bound here, so a watch may connect before the node has begun.
    let bound = runtime.block_on(async { node::bind(socket) })?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let steers = socket.with_file_name("steer");
    let running = thread::spawn(move || {
        let realm = Realm {
            name: "bench".to_owned(),
            machine: "bench".to_owned(),
        };
        let until_stopped = async {
            // Sent or dropped, either way the bench is done.
            let _ = stopped.await;
        };
        runtime.block_on(node::run(
            store,
            realm,
            Listed::<Ssh>(Vec::new()),
            node::Given {
                sharing: None,
                starter: None,
                refusals: Refusals::default(),
                steers,
                awake: Awake::by(None),
                updater: "/nowhere".into(),
            },
            bound,
            until_stopped,
        ))
    });
    Ok((stop, running))
}

/// Links `near`, a second machine's folders, with `far`, the node's, over
/// a pipe, on a thread of its own until the sender answered is sent to or
/// dropped.
fn run_link(near: Folders, far: Folders) -> io::Result<Running> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = thread::spawn(move || {
        let (one, two) = tokio::io::duplex(PIPE);
        runtime.block_on(async {
            let ends = async { tokio::try_join!(end(near, "near", one), end(far, "bench", two)) };
            tokio::select! {
                ended = ends => ended.map(|_| ()),
                // Sent or dropped, either way the bench is done.
                _ = stopped => Ok(()),
            }
        })
    });
    Ok((stop, running))
}

/// Runs `machine`'s end of a link over `stream` from `store`.
async fn end(store: Folders, machine: &str, stream: tokio::io::DuplexStream) -> io::Result<()> {
    let (reading, writing) = tokio::io::split(stream);
    let realm = Realm {
        name: "bench".to_owned(),
        machine: machine.to_owned(),
    };
    let connection = Connection {
        reading: Box::new(reading),
        writing: Box::new(writing),
        keep: Box::new(()),
        refused: None,
        releases: false,
        hello_within: Some(link::dial::HELLO_WITHIN),
    };
    link::run(store, &realm, connection, None, None)
        .await
        .map(|_| ())
}

/// Watches group `bench.g<watch>` on the socket at `socket`, once the node
/// has begun the watch, on a thread of its own that tells `heard` how long
/// each post took, timed from `start`, until the node hangs up.
fn watch_group(
    socket: &Path,
    watch: u32,
    start: Instant,
    heard: mpsc::Sender<Heard>,
) -> Result<JoinHandle<()>, Box<dyn Error>> {
    let mut client = Client::connect(socket)?;
    let groups = Pattern::new(format!("bench.g{watch}"))?;
    match client.ask(&Request::Watch {
        groups: Some(groups),
        recent: 0,
    })? {
        Ok(Response::Watching) => {}
        other => return Err(format!("the node didn't watch: {other:?}").into()),
    }
    // The whole state comes first, once the watch has begun.
    for _ in 0..2 {
        client.next_event()?.ok_or("the node hung up")?;
    }
    Ok(thread::spawn(move || {
        while let Ok(Some(event)) = client.next_event() {
            let Event::Message { message } = event else {
                continue;
            };
            let now = start.elapsed();
            if let Some(post) = Post::read(&message.text) {
                // Nobody hears once the bench is done.
                let _ = heard.send((post.index, now.saturating_sub(post.written)));
            }
        }
    }))
}

/// Writes `total` posts into `store` at the rate asked, as a session
/// writes a message, each timed from `start`, answering how long each
/// write after the first second's took.
fn post(
    asked: &Asked,
    store: &Folders,
    start: Instant,
    total: u32,
) -> Result<Vec<Duration>, Box<dyn Error>> {
    let mut writes = Vec::new();
    let begun = Instant::now();
    for index in 0..total {
        let due = begun + Duration::from_secs(1) * index / asked.rate;
        thread::sleep(due.saturating_duration_since(Instant::now()));
        let draft = Draft {
            from: format!("s{}", index % asked.senders),
            groups: vec![Group::new(format!("bench.g{}", index % asked.watches))?],
            intent: None,
            text: Post {
                index,
                written: start.elapsed(),
            }
            .text(),
            re: None,
        };
        let (id, body) = mesh::written(&draft, std::process::id());
        let writing = Instant::now();
        store.put_message(&id, &body)?;
        if index >= asked.rate {
            writes.push(writing.elapsed());
        }
    }
    Ok(writes)
}

/// Takes what is heard until all `total` posts are, or the stragglers'
/// wait runs out, answering how many were and how long each after the
/// first second's took.
fn gather(hearing: &mpsc::Receiver<Heard>, total: u32, rate: u32) -> (u32, Vec<Duration>) {
    let mut times = Vec::new();
    let mut got = 0;
    let deadline = Instant::now() + STRAGGLERS_WITHIN;
    while got < total {
        let Ok((index, took)) =
            hearing.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        else {
            break;
        };
        got += 1;
        if index >= rate {
            times.push(took);
        }
    }
    (got, times)
}

/// Prints what was asked and what was measured, `times` and `writes`
/// sorted.
fn report(asked: &Asked, got: u32, total: u32, times: &[Duration], writes: &[Duration]) {
    println!(
        "{} watches, each on a group of its own; {} senders; {} posts a second for {} s{}{}",
        asked.watches,
        asked.senders,
        asked.rate,
        asked.seconds,
        if asked.across { "; across a link" } else { "" },
        load().map_or(String::new(), |load| format!("; load {load}")),
    );
    println!(
        "posts heard: {got} of {total}; the first second's {} left out",
        asked.rate.min(total)
    );
    for (what, times) in [("written to heard", times), ("the write alone", writes)] {
        if !times.is_empty() {
            println!(
                "{what}: p50 {:?}, p99 {:?}, p99.9 {:?}, max {:?}",
                at(times, 500),
                at(times, 990),
                at(times, 999),
                at(times, 1000),
            );
        }
    }
}

/// A post, as its text carries it: which it is, and when it was written.
struct Post {
    index: u32,
    written: Duration,
}

impl Post {
    fn text(&self) -> String {
        format!("{} {}", self.index, self.written.as_nanos())
    }

    fn read(text: &str) -> Option<Self> {
        let (index, written) = text.split_once(' ')?;
        Some(Self {
            index: index.parse().ok()?,
            written: Duration::from_nanos(written.parse().ok()?),
        })
    }
}

/// The time `per_mille` of the way along `times`, sorted.
fn at(times: &[Duration], per_mille: usize) -> Duration {
    times[(times.len() - 1) * per_mille / 1000]
}

/// The machine's load over the last minute, where it says.
fn load() -> Option<String> {
    let text = fs::read_to_string("/proc/loadavg").ok()?;
    text.split_whitespace().next().map(str::to_owned)
}
