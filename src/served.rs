//! Files the node serves as kinds of the daemon interface, so a reader
//! asks the node for what another program keeps in a file, and hears each
//! change, without the node learning that file's shape. The files stay
//! their own record: the node's configuration names each kind's file or
//! folder, the field holding its records and the field keying each, and
//! the node passes every record's fields on as they are.
//!
//! A source is read whole when a change reaches its folder, as the
//! program keeping it writes it whole and renames it into place, and its
//! records change on the wire only by what differs.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use notify::Watcher;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::daemon;

/// How long a source whose folder can't be watched waits before trying
/// again, as a folder its program hasn't made yet.
const AGAIN_AFTER: Duration = Duration::from_secs(60);

/// How long after a change the node waits for more before reading, so a
/// burst of writes is read once.
const SETTLED_AFTER: Duration = Duration::from_millis(20);

/// Where one kind's records are kept, as the configuration names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// The one file holding them, by its full path, or
    #[serde(default)]
    pub file: Option<PathBuf>,
    /// a folder of `.json` files, each holding some, by its full path.
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// The field of each file holding its records: an array of objects,
    /// or an object of them by their keys.
    pub records: String,
    /// The field keying each record in an array. An object's records are
    /// keyed by their own keys.
    #[serde(default)]
    pub key: Option<String>,
    /// For a folder, the field of each file put in front of its records'
    /// keys, so two files' records never share one.
    #[serde(default)]
    pub by: Option<String>,
}

impl Source {
    /// The file or folder it names.
    fn path(&self) -> &Path {
        self.file
            .as_deref()
            .or(self.folder.as_deref())
            .unwrap_or(Path::new(""))
    }

    /// The folder whose changes may change it: the file's own folder, as
    /// a file renamed into place is a new file there, or the folder.
    fn watched(&self) -> &Path {
        match (&self.file, &self.folder) {
            (Some(file), _) => file.parent().unwrap_or(Path::new("/")),
            (None, Some(folder)) => folder,
            (None, None) => Path::new(""),
        }
    }

    /// Whether a change to `path` may change its records: its file, or for
    /// a folder, one of its record files or the folder itself going.
    fn touched_by(&self, path: &Path) -> bool {
        // A Mac names paths as the system resolves them, which may not be
        // how they were given, so only names are compared.
        let name = path.file_name();
        match &self.file {
            Some(file) => name.is_some() && name == file.file_name(),
            None => is_record_file(path) || (name.is_some() && name == self.watched().file_name()),
        }
    }
}

/// The kinds a configuration file names, as a JSON object of sources by
/// kind, refusing one named like a kind the node holds itself, in `held`.
///
/// # Errors
///
/// When the file can't be read, isn't sources by kind, or names one
/// whose kind or path can't be served.
pub fn configured(file: &Path, held: &[&str]) -> Result<BTreeMap<String, Source>, String> {
    let read = fs::read(file).map_err(|error| format!("{}: {error}", file.display()))?;
    let sources: BTreeMap<String, Source> = serde_json::from_slice(&read)
        .map_err(|error| format!("{} isn't sources by kind: {error}", file.display()))?;
    for (kind, source) in &sources {
        let refused = |why: &str| Err(format!("{} can't serve {kind:?}: {why}", file.display()));
        if kind.is_empty() || kind.contains(':') {
            return refused("a kind is a name with no `:`");
        }
        if held.contains(&kind.as_str()) {
            return refused("the node holds that kind itself");
        }
        if source.file.is_some() == source.folder.is_some() {
            return refused("it names a `file` or a `folder`, and only one");
        }
        if !source.path().is_absolute() {
            return refused("its path is a full one, as no folder is the node's to start from");
        }
        if source.by.is_some() && source.folder.is_none() {
            return refused("only a folder's files have a `by`");
        }
    }
    Ok(sources)
}

/// What a source held when last read: its records by key, and why any of
/// it couldn't be read.
#[derive(Debug, Clone, Default, PartialEq)]
struct Read {
    records: BTreeMap<String, Value>,
    refused: Option<String>,
}

/// Reads `kind`'s records from `source` now.
fn read(kind: &str, source: &Source) -> Read {
    match (&source.file, &source.folder) {
        (Some(file), _) => match read_file(kind, source, file) {
            Ok(records) => Read {
                records,
                refused: None,
            },
            Err(why) => Read {
                records: BTreeMap::new(),
                refused: Some(why),
            },
        },
        (None, Some(folder)) => read_folder(kind, source, folder),
        (None, None) => Read::default(),
    }
}

/// Reads every record file in `folder`, keeping those that read and saying
/// why any didn't.
fn read_folder(kind: &str, source: &Source, folder: &Path) -> Read {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Read::default(),
        Err(error) => {
            return Read {
                records: BTreeMap::new(),
                refused: Some(format!("{}: {error}", folder.display())),
            };
        }
    };
    let mut read = Read::default();
    let mut refused = Vec::new();
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_record_file(path))
        .collect();
    files.sort();
    for file in files {
        match read_file(kind, source, &file) {
            Ok(records) => read.records.extend(records),
            Err(why) => refused.push(why),
        }
    }
    if !refused.is_empty() {
        read.refused = Some(refused.join("; "));
    }
    read
}

/// Whether `path` is a file a folder's records may be in: a `.json` file,
/// and not one hidden, as a file being written before its rename is.
fn is_record_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "json")
        && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
}

/// Reads `file`'s records as `kind`'s, each keyed as `source` says, heard
/// when the file was last written.
fn read_file(kind: &str, source: &Source, file: &Path) -> Result<BTreeMap<String, Value>, String> {
    let named = |why: String| format!("{}: {why}", file.display());
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        // Not written yet, or removed: it holds nothing, which is no trouble.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(named(error.to_string())),
    };
    let heard = fs::metadata(file)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|since| i64::try_from(since.as_millis()).ok())
        .unwrap_or_default();
    let whole: Value =
        serde_json::from_slice(&bytes).map_err(|error| named(format!("not JSON: {error}")))?;
    let front = match &source.by {
        Some(by) => Some(
            whole
                .get(by)
                .and_then(id_of)
                .ok_or_else(|| named(format!("no `{by}` to key its records by")))?,
        ),
        None => None,
    };
    let keyed = |id: String| match &front {
        Some(front) => format!("{front}:{id}"),
        None => id,
    };
    let records = match whole.get(&source.records) {
        Some(Value::Array(records)) => {
            let key = source.key.as_deref().ok_or_else(|| {
                named(format!(
                    "`{}` is a list, so it needs a `key`",
                    source.records
                ))
            })?;
            records
                .iter()
                .map(|record| {
                    let id = record
                        .get(key)
                        .and_then(id_of)
                        .ok_or_else(|| named(format!("a record has no `{key}`")))?;
                    Ok((keyed(id), record.clone()))
                })
                .collect::<Result<Vec<_>, String>>()?
        }
        Some(Value::Object(records)) => records
            .iter()
            .map(|(id, record)| (keyed(id.clone()), record.clone()))
            .collect(),
        _ => return Err(named(format!("no `{}` of records", source.records))),
    };
    Ok(records
        .into_iter()
        .map(|(id, fields)| {
            let record = daemon::record(kind, &id, heard, fields);
            (daemon::key(kind, &id), record)
        })
        .collect())
}

/// A field's value as an id: a string as it is, a number written out.
fn id_of(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

/// The kinds the node serves from files, each kept read as its source
/// changes, for as long as this is kept.
#[derive(Debug, Default)]
pub struct Served {
    sources: BTreeMap<String, Source>,
    held: Arc<Mutex<BTreeMap<String, Read>>>,
    told: BTreeMap<String, watch::Sender<u64>>,
    keeping: Vec<tokio::task::JoinHandle<()>>,
}

impl Served {
    /// Serves each of `sources`, keeping each read as it changes, on the
    /// runtime this runs on.
    #[must_use]
    pub fn start(sources: BTreeMap<String, Source>) -> Self {
        let held: Arc<Mutex<BTreeMap<String, Read>>> = Arc::default();
        let mut told = BTreeMap::new();
        let mut keeping = Vec::new();
        for (kind, source) in &sources {
            let (tell, _) = watch::channel(0);
            keeping.push(tokio::spawn(keep(
                kind.clone(),
                source.clone(),
                Arc::clone(&held),
                tell.clone(),
            )));
            told.insert(kind.clone(), tell);
        }
        Self {
            sources,
            held,
            told,
            keeping,
        }
    }

    /// The kinds it serves.
    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.sources.keys().map(String::as_str)
    }

    /// Every record of `kind` it holds now, by key, or none for a kind it
    /// doesn't serve.
    #[must_use]
    pub fn records(&self, kind: &str) -> Option<BTreeMap<String, Value>> {
        self.sources.get(kind)?;
        Some(
            self.lock()
                .get(kind)
                .map(|read| read.records.clone())
                .unwrap_or_default(),
        )
    }

    /// What changes each time `kind`'s records may have, or none for a
    /// kind it doesn't serve.
    #[must_use]
    pub fn told(&self, kind: &str) -> Option<watch::Receiver<u64>> {
        self.told.get(kind).map(watch::Sender::subscribe)
    }

    /// How each source stands, as the interface's `health` names a source:
    /// answering, or refused with why.
    #[must_use]
    pub fn health(&self) -> Vec<Value> {
        let held = self.lock();
        self.sources
            .iter()
            .map(|(kind, source)| {
                let file = source.path().display().to_string();
                match held.get(kind).and_then(|read| read.refused.as_ref()) {
                    Some(why) => {
                        json!({"name": kind, "file": file, "state": "refused", "why": why})
                    }
                    None => json!({"name": kind, "file": file, "state": "answering"}),
                }
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Read>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        for keeping in &self.keeping {
            keeping.abort();
        }
    }
}

/// Keeps `kind`'s records read from `source` in `held`, telling `tell`
/// each time they change: read once its folder is watched, then again
/// after each change there, and watched again once the folder is made
/// anew or after [`AGAIN_AFTER`] while it can't be.
async fn keep(
    kind: String,
    source: Source,
    held: Arc<Mutex<BTreeMap<String, Read>>>,
    tell: watch::Sender<u64>,
) {
    let keep = |read: Read| {
        let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.get(&kind) != Some(&read) {
            if let Some(why) = &read.refused {
                eprintln!("serving {kind}: {why}");
            }
            held.insert(kind.clone(), read);
            tell.send_modify(|changes| *changes += 1);
        }
    };
    loop {
        let (watcher, mut changed) = match watching(&source) {
            Ok(watching) => watching,
            Err(error) => {
                // A folder not made yet holds nothing, which is no trouble.
                let refused = source
                    .watched()
                    .exists()
                    .then(|| format!("{} can't be watched: {error}", source.watched().display()));
                keep(Read {
                    records: BTreeMap::new(),
                    refused,
                });
                tokio::time::sleep(AGAIN_AFTER).await;
                continue;
            }
        };
        keep(read(&kind, &source));
        while changed.recv().await.is_some() {
            tokio::time::sleep(SETTLED_AFTER).await;
            while changed.try_recv().is_ok() {}
            keep(read(&kind, &source));
            if !source.watched().is_dir() {
                break;
            }
        }
        drop(watcher);
    }
}

/// Watches `source`'s folder, telling what this answers each time a change
/// there may change its records. The watch lasts as long as the watcher
/// answered is kept.
fn watching(
    source: &Source,
) -> io::Result<(notify::RecommendedWatcher, mpsc::UnboundedReceiver<()>)> {
    let (tell, told) = mpsc::unbounded_channel();
    let touched = source.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if changes(&touched, &event) {
            // Nothing hears this once the source is no longer served.
            let _ = tell.send(());
        }
    })
    .map_err(io::Error::other)?;
    watcher
        .watch(source.watched(), notify::RecursiveMode::NonRecursive)
        .map_err(io::Error::other)?;
    Ok((watcher, told))
}

/// Whether `event` may change `source`'s records: one of its files
/// written, renamed or removed, or events missed. Reading one changes
/// nothing, and a Linux watch hears the node's own reads too, which would
/// otherwise have it read again without end.
fn changes(source: &Source, event: &notify::Result<notify::Event>) -> bool {
    let Ok(event) = event else {
        return true;
    };
    if event.need_rescan() {
        return true;
    }
    let only_looked = matches!(
        event.kind,
        notify::EventKind::Access(_)
            | notify::EventKind::Modify(notify::event::ModifyKind::Metadata(_))
    );
    !only_looked && event.paths.iter().any(|path| source.touched_by(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How long a test waits for the node to hear a change.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// Writes `value` to `file` whole, as a program keeping a served file
    /// does: beside it, then renamed into place.
    fn write(file: &Path, value: &Value) {
        let beside = file.with_file_name(format!(
            ".{}.new",
            file.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file")
        ));
        fs::write(&beside, value.to_string()).expect("written beside");
        fs::rename(&beside, file).expect("renamed into place");
    }

    fn quests(file: &Path) -> Source {
        Source {
            file: Some(file.to_owned()),
            folder: None,
            records: "quests".to_owned(),
            key: Some("id".to_owned()),
            by: None,
        }
    }

    /// The fields of `records` other than the interface's own `heard`,
    /// which is when the file was written.
    fn unheard(records: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
        records
            .iter()
            .map(|(key, record)| {
                let mut record = record.clone();
                record.as_object_mut().map(|fields| fields.remove("heard"));
                (key.clone(), record)
            })
            .collect()
    }

    #[test]
    fn a_file_is_served_as_records_keyed_by_a_field_or_by_their_own_keys() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let file = dir.path().join("quests.json");
        write(
            &file,
            &json!({"quests": [{"id": "QEST-A", "title": "a"}, {"id": 7, "title": "b"}]}),
        );
        let quest = read("quest", &quests(&file));
        assert_eq!(quest.refused, None, "it reads");
        assert_eq!(
            unheard(&quest.records),
            BTreeMap::from([
                (
                    "quest:7".to_owned(),
                    json!({"key": "quest:7", "kind": "quest", "id": 7, "title": "b"})
                ),
                (
                    "quest:QEST-A".to_owned(),
                    json!({"key": "quest:QEST-A", "kind": "quest", "id": "QEST-A", "title": "a"})
                ),
            ]),
            "each record keyed by its `id`, its fields as they are"
        );

        let verdicts = dir.path().join("verdicts.json");
        write(&verdicts, &json!({"items": {"QEST-A:1": {"said": "yes"}}}));
        let source = Source {
            file: Some(verdicts),
            records: "items".to_owned(),
            key: None,
            ..quests(&file)
        };
        assert_eq!(
            unheard(&read("verdict", &source).records),
            BTreeMap::from([(
                "verdict:QEST-A:1".to_owned(),
                json!({"key": "verdict:QEST-A:1", "kind": "verdict", "said": "yes"})
            )]),
            "an object's records are keyed by their own keys"
        );
    }

    #[test]
    fn a_folders_files_are_served_each_keyed_by_its_field_and_one_that_wont_read_is_said() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let waiting = Source {
            file: None,
            folder: Some(dir.path().to_owned()),
            records: "items".to_owned(),
            key: Some("id".to_owned()),
            by: Some("quest".to_owned()),
        };
        write(
            &dir.path().join("A.json"),
            &json!({"quest": "QEST-A", "items": [{"id": "login"}]}),
        );
        write(
            &dir.path().join("B.json"),
            &json!({"quest": "QEST-B", "items": [{"id": "login"}]}),
        );
        fs::write(dir.path().join("C.json"), "{half").expect("written");
        let held = read("waiting", &waiting);
        assert_eq!(
            held.records.keys().collect::<Vec<_>>(),
            ["waiting:QEST-A:login", "waiting:QEST-B:login"],
            "two files' records never share a key"
        );
        assert!(
            held.refused
                .as_ref()
                .is_some_and(|why| why.contains("C.json") && why.contains("not JSON")),
            "the file that won't read is named: {:?}",
            held.refused
        );
    }

    #[test]
    fn a_configuration_serves_only_what_it_can() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let config = dir.path().join("serves.json");
        let refuses = |sources: Value| {
            fs::write(&config, sources.to_string()).expect("written");
            configured(&config, &["session", "machine"]).expect_err("refused")
        };
        assert!(
            refuses(json!({"session": {"file": "/a.json", "records": "r"}}))
                .contains("holds that kind itself"),
            "a kind the node holds is its own"
        );
        assert!(
            refuses(json!({"quest": {"file": "a.json", "records": "r"}})).contains("full one"),
            "a path is a full one"
        );
        assert!(
            refuses(json!({"quest": {"file": "/a", "folder": "/b", "records": "r"}}))
                .contains("only one"),
            "a file or a folder"
        );
        fs::write(
            &config,
            json!({"quest": {"file": "/q.json", "records": "quests", "key": "id"}}).to_string(),
        )
        .expect("written");
        assert_eq!(
            configured(&config, &["session"]),
            Ok(BTreeMap::from([(
                "quest".to_owned(),
                quests(Path::new("/q.json"))
            )])),
            "what it can serve is served"
        );
    }

    /// Waits until `served` holds `kind`'s records as `wanted` says, telling
    /// `told` each time they may have changed.
    async fn until(
        served: &Served,
        told: &mut watch::Receiver<u64>,
        kind: &str,
        wanted: impl Fn(&BTreeMap<String, Value>) -> bool,
        what: &str,
    ) {
        tokio::time::timeout(PATIENCE, async {
            while !wanted(&served.records(kind).unwrap_or_default()) {
                told.changed().await.expect("still served");
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}: {:?}", served.records(kind)));
    }

    #[tokio::test]
    async fn a_served_file_is_read_again_as_it_changes_and_its_records_go_with_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let file = dir.path().join("quests.json");
        write(&file, &json!({"quests": [{"id": "A", "title": "one"}]}));
        let served = Served::start(BTreeMap::from([("quest".to_owned(), quests(&file))]));
        let mut told = served.told("quest").expect("quest is served");
        let title = |records: &BTreeMap<String, Value>| {
            records.get("quest:A").map(|record| record["title"].clone())
        };
        until(
            &served,
            &mut told,
            "quest",
            |r| title(r) == Some(json!("one")),
            "it is read",
        )
        .await;

        write(&file, &json!({"quests": [{"id": "A", "title": "two"}]}));
        until(
            &served,
            &mut told,
            "quest",
            |r| title(r) == Some(json!("two")),
            "read again",
        )
        .await;

        fs::remove_file(&file).expect("removed");
        until(
            &served,
            &mut told,
            "quest",
            BTreeMap::is_empty,
            "its records go",
        )
        .await;
        assert_eq!(
            served.health()[0]["state"],
            "answering",
            "a file gone holds nothing, which is no trouble: {:?}",
            served.health()
        );
    }

    #[tokio::test]
    async fn a_source_not_made_yet_holds_nothing_and_is_no_trouble() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let file = dir.path().join("quests.json");
        assert_eq!(
            read("quest", &quests(&file)),
            Read::default(),
            "a file not written yet"
        );
        let waiting = Source {
            file: None,
            folder: Some(dir.path().join("mission/waiting")),
            records: "items".to_owned(),
            key: Some("id".to_owned()),
            by: Some("quest".to_owned()),
        };
        assert_eq!(
            read("waiting", &waiting),
            Read::default(),
            "a folder not made yet"
        );

        let served = Served::start(BTreeMap::from([("waiting".to_owned(), waiting)]));
        let mut told = served.told("waiting").expect("waiting is served");
        tokio::time::timeout(PATIENCE, told.changed())
            .await
            .expect("its first look is kept")
            .expect("still served");
        assert_eq!(
            served.health()[0]["state"],
            "answering",
            "a folder it can't watch, as it isn't there, is no trouble: {:?}",
            served.health()
        );
    }
}
