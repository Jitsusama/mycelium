//! What a model call lent may spend: each model's price, as the lend
//! file says it, the most a request may cost, reserved before it is
//! sent, and what its answer says it used, read as it streams, to
//! settle it by; and spend-watch's word on the budget it spends.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::{Price, Refusal, refused, unparsed};

/// Where spend-watch says how each budget stands, within the account's
/// runtime folder.
pub(super) const SPEND_WATCH: &str = "spend-watch/budgets.json";

/// How old spend-watch's word may be before it reads as silent, which
/// refuses every model lend. It writes every few minutes.
const SPEND_WATCH_WITHIN: jiff::SignedDuration = jiff::SignedDuration::from_mins(15);

/// How far ahead of this machine's clock spend-watch's word may say it
/// was written, as two clocks a moment apart.
const SPEND_WATCH_AHEAD: jiff::SignedDuration = jiff::SignedDuration::from_mins(1);

/// The most of one line of a model's streamed answer read for its usage;
/// a longer one is passed over, as its usage never is.
const MOST_METERED_LINE: usize = 256 * 1024;

/// The most of a model's answer that isn't streamed read for its usage;
/// a longer one is charged its reservation.
const MOST_METERED: usize = 8 * 1024 * 1024;

impl Price {
    fn cache_write(self) -> f64 {
        self.cache_write.unwrap_or(self.input * 1.25)
    }

    fn cache_read(self) -> f64 {
        self.cache_read.unwrap_or(self.input)
    }

    /// The most tokens a request may ask it to answer with.
    pub(super) const fn most_output(self) -> u64 {
        self.most_output
    }

    /// Whether every price is one: finite and not below nothing, so none
    /// can reserve less than a request may cost.
    pub(super) fn is_one(self) -> bool {
        [
            self.input,
            self.output,
            self.cache_write(),
            self.cache_read(),
        ]
        .iter()
        .all(|each| each.is_finite() && *each >= 0.0)
    }

    /// The most a request can cost, in millionths of a dollar: each of
    /// the `asked` bytes of its body as a token at the dearest a token
    /// asked can cost, as no token is shorter than a byte, and its `most`
    /// tokens answered.
    pub(super) fn reserved(self, asked: u64, most: u64) -> u64 {
        let dearest = self.input.max(self.cache_write()).max(self.cache_read());
        micros(dearest * tokens(asked) + self.output * tokens(most))
    }

    /// What `usage` cost, in millionths of a dollar, its tokens asked
    /// counted as `asked` where the answer said none.
    pub(super) fn charged(self, usage: &Usage, asked: u64) -> u64 {
        let input = usage.input.unwrap_or(asked);
        micros(
            self.input * tokens(input)
                + self.cache_write() * tokens(usage.cache_write)
                + self.cache_read() * tokens(usage.cache_read)
                + self.output * tokens(usage.output),
        )
    }
}

/// A count of tokens, to be priced.
#[expect(
    clippy::cast_precision_loss,
    reason = "no request's tokens come near 2^52"
)]
fn tokens(count: u64) -> f64 {
    count as f64
}

/// Dollars a million tokens times tokens, in millionths of a dollar,
/// rounded up, so a charge is never under what it cost.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a cost is never below nothing, and one past u64's millionths of a dollar is \
              saturated, as `as` does"
)]
pub(super) fn micros(cost: f64) -> u64 {
    cost.ceil().max(0.0) as u64
}

/// Millionths of a dollar as dollars, for a line or a refusal.
#[expect(
    clippy::cast_precision_loss,
    reason = "a day's spend is far below 2^52 millionths of a dollar"
)]
pub(super) fn dollars(micros: u64) -> f64 {
    micros as f64 / 1_000_000.0
}

/// A model's tokens an answer said it used.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Usage {
    /// Those asked, not from a cache, where it said.
    input: Option<u64>,
    /// Those written to its cache.
    cache_write: u64,
    /// Those read from its cache.
    cache_read: u64,
    /// Those answered.
    output: u64,
}

/// A model's answer, read for the tokens it says it used as it streams:
/// in its server-sent events, each `data:` line's JSON, or else in the
/// whole body's. Only `usage` where the upstream's own JSON puts it is
/// read, at the top or in a `message`, so nothing a model says in its
/// text, which is always within a string, can stand for it.
#[derive(Debug, Default)]
pub(super) struct Meter {
    /// Whether the answer is a stream of events.
    events: bool,
    /// The line of events being read.
    line: Capped<MOST_METERED_LINE>,
    /// The answer, where it isn't a stream of events.
    body: Capped<MOST_METERED>,
    /// The tokens it says it used, the most each count said.
    usage: Usage,
    /// Whether it said how many it answered with.
    counted: bool,
    /// Whether it said it was done: a stream's `message_stop` or
    /// `[DONE]`, or a whole body read.
    finished: bool,
}

impl Meter {
    /// Reads the answer's headers, which say whether it streams events.
    pub(super) fn headed(&mut self, headers: &[(String, String)]) {
        self.events = headers.iter().any(|(name, value)| {
            name == "content-type" && value.to_ascii_lowercase().starts_with("text/event-stream")
        });
    }

    /// Reads a piece of the answer's body.
    pub(super) fn read(&mut self, piece: &[u8]) {
        if !self.events {
            self.body.push(piece);
            return;
        }
        for part in piece.split_inclusive(|byte| *byte == b'\n') {
            if let Some(ended) = part.strip_suffix(b"\n") {
                self.line.push(ended);
                self.line_read();
            } else {
                self.line.push(part);
            }
        }
    }

    /// Reads the event line just ended.
    fn line_read(&mut self) {
        let Some(line) = self.line.take() else {
            return;
        };
        let line = line.strip_suffix(b"\r").unwrap_or(&line);
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        let data = data.trim_ascii();
        if data == b"[DONE]" {
            self.finished = true;
        } else if let Ok(event) = serde_json::from_slice::<serde_json::Value>(data) {
            self.said(&event);
        }
    }

    /// Reads what one event, or a whole answer, says.
    fn said(&mut self, said: &serde_json::Value) {
        if said.get("type").and_then(serde_json::Value::as_str) == Some("message_stop") {
            self.finished = true;
        }
        for usage in [said.get("usage"), said.pointer("/message/usage")]
            .into_iter()
            .flatten()
        {
            let count = |names: &[&str]| {
                names
                    .iter()
                    .filter_map(|name| usage.get(*name).and_then(serde_json::Value::as_u64))
                    .max()
            };
            if let Some(input) = count(&["input_tokens", "prompt_tokens"]) {
                self.usage.input = Some(self.usage.input.unwrap_or_default().max(input));
            }
            if let Some(output) = count(&["output_tokens", "completion_tokens"]) {
                self.usage.output = self.usage.output.max(output);
                self.counted = true;
            }
            if let Some(written) = count(&["cache_creation_input_tokens"]) {
                self.usage.cache_write = self.usage.cache_write.max(written);
            }
            if let Some(read) = count(&["cache_read_input_tokens"]) {
                self.usage.cache_read = self.usage.cache_read.max(read);
            }
        }
    }

    /// Reads what is left once the answer was sent whole.
    pub(super) fn ended(&mut self) {
        if self.events {
            if !self.line.is_empty() {
                self.line_read();
            }
        } else if let Some(body) = self.body.take()
            && let Ok(whole) = serde_json::from_slice::<serde_json::Value>(&body)
        {
            self.said(&whole);
            self.finished = true;
        }
    }

    /// The tokens the answer said it used, where it said so and that it
    /// was done.
    pub(super) fn usage(&self) -> Option<&Usage> {
        (self.finished && self.counted).then_some(&self.usage)
    }
}

/// Bytes kept up to `MOST`, and once more came, none: what ran past is
/// never read in part.
#[derive(Debug, Default)]
struct Capped<const MOST: usize> {
    /// What was kept.
    bytes: Vec<u8>,
    /// Whether more came than `MOST`.
    over: bool,
}

impl<const MOST: usize> Capped<MOST> {
    /// Keeps `piece`, or none of what came once it runs past `MOST`.
    fn push(&mut self, piece: &[u8]) {
        if self.over {
            return;
        }
        if self.bytes.len() + piece.len() > MOST {
            self.over = true;
            self.bytes = Vec::new();
        } else {
            self.bytes.extend_from_slice(piece);
        }
    }

    /// Whether nothing came since it was last taken.
    const fn is_empty(&self) -> bool {
        self.bytes.is_empty() && !self.over
    }

    /// What came since it was last taken, unless it ran past `MOST`;
    /// either way it starts afresh.
    fn take(&mut self) -> Option<Vec<u8>> {
        let over = std::mem::take(&mut self.over);
        let bytes = std::mem::take(&mut self.bytes);
        (!over).then_some(bytes)
    }
}

/// Whether spend-watch, in `runtime`'s [`SPEND_WATCH`], lets a model's
/// request spend the budget `named` there: refused 503 where it is
/// silent, its word missing, unreadable, more than [`SPEND_WATCH_WITHIN`]
/// old or silent of `named`, and 429 where it says `named` is past an
/// allowance, as one its provider refuses or one spent.
pub(super) fn spend_watch_allows(runtime: Option<&Path>, named: &str) -> Result<(), Refusal> {
    let silent = |why: String| {
        refused(
            503,
            format!("spend-watch is silent, so no model is lent: {why}"),
        )
    };
    let file = runtime
        .ok_or_else(|| silent("this machine has no runtime folder to find it in".to_owned()))?
        .join(SPEND_WATCH);
    let text =
        fs::read(&file).map_err(|error| silent(format!("its word can't be read: {error}")))?;
    let said: Said = serde_json::from_slice(&text)
        .map_err(|error| silent(format!("its word {}", unparsed(&error))))?;
    let old = jiff::Timestamp::now().duration_since(said.at);
    if old > SPEND_WATCH_WITHIN || old < -SPEND_WATCH_AHEAD {
        return Err(silent(format!(
            "its word is from {}, not within {} minutes",
            said.at,
            SPEND_WATCH_WITHIN.as_mins()
        )));
    }
    let standing = said
        .budgets
        .iter()
        .find(|budget| budget.name == named)
        .ok_or_else(|| silent(format!("it says nothing of the budget {named}")))?;
    match standing
        .allowances
        .iter()
        .find(|allowance| allowance.past())
    {
        Some(allowance) => Err(refused(
            429,
            format!(
                "spend-watch says the budget {named} is past its {} allowance",
                allowance.label.as_deref().unwrap_or("own")
            ),
        )),
        None => Ok(()),
    }
}

/// What spend-watch said of the budgets, as far as a lend reads it.
#[derive(Deserialize)]
struct Said {
    at: jiff::Timestamp,
    budgets: Vec<Standing>,
}

/// How one budget stands, as spend-watch says it.
#[derive(Deserialize)]
struct Standing {
    name: String,
    #[serde(default)]
    allowances: Vec<Allowance>,
}

/// One of a budget's allowances, as spend-watch says it.
#[derive(Deserialize)]
struct Allowance {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default, rename = "allowance")]
    most: Option<f64>,
    #[serde(default)]
    spent: Option<f64>,
    #[serde(default)]
    used: Option<f64>,
}

impl Allowance {
    /// Whether it is past: its provider refuses, it is spent, or as a
    /// share it is used whole.
    fn past(&self) -> bool {
        self.state.as_deref() == Some("refused")
            || matches!((self.spent, self.most), (Some(spent), Some(most)) if spent >= most)
            || (self.unit.as_deref() == Some("share") && self.used.is_some_and(|used| used >= 1.0))
    }
}
