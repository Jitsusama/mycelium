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

/// What a model's tokens cost, each in millionths of a dollar a million
/// tokens, so every charge is whole numbers, worked out exactly.
#[derive(Clone, Copy)]
struct Rates {
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
}

impl Price {
    /// Its prices in whole millionths, where every one is a price: a cache
    /// write a quarter more than `input` where none is given, rounded up,
    /// and a cache read `input`.
    fn rates(self) -> Option<Rates> {
        let input = millionths(self.input)?;
        let cache_write = match self.cache_write {
            Some(given) => millionths(given)?,
            None => input.checked_add(input.div_ceil(4))?,
        };
        let cache_read = match self.cache_read {
            Some(given) => millionths(given)?,
            None => input,
        };
        Some(Rates {
            input,
            output: millionths(self.output)?,
            cache_write,
            cache_read,
        })
    }

    /// The most tokens a request may ask it to answer with.
    pub(super) const fn most_output(self) -> u64 {
        self.most_output
    }

    /// Whether every price is one: finite, not below nothing and within
    /// what a count of millionths holds, so none can reserve less than a
    /// request may cost.
    pub(super) fn is_one(self) -> bool {
        self.rates().is_some()
    }

    /// The most a request can cost, in millionths of a dollar: each of
    /// the `asked` bytes of its body as a token at the dearest a token
    /// asked can cost, as no token is shorter than a byte, and its `most`
    /// tokens answered. A price that isn't one reserves everything.
    pub(super) fn reserved(self, asked: u64, most: u64) -> u64 {
        let Some(rates) = self.rates() else {
            return u64::MAX;
        };
        let dearest = rates.input.max(rates.cache_write).max(rates.cache_read);
        micros(&[(dearest, asked), (rates.output, most)])
    }

    /// What `usage` cost, in millionths of a dollar, its tokens asked
    /// counted as `asked` where the answer said none. A price that isn't
    /// one charges everything.
    pub(super) fn charged(self, usage: &Usage, asked: u64) -> u64 {
        let Some(rates) = self.rates() else {
            return u64::MAX;
        };
        micros(&[
            (rates.input, usage.input.unwrap_or(asked)),
            (rates.cache_write, usage.cache_write),
            (rates.cache_read, usage.cache_read),
            (rates.output, usage.output),
        ])
    }

    /// What an answer cut short can have cost, in millionths of a dollar:
    /// the tokens its upstream said were `asked`, each at its own price,
    /// and its `most` tokens answered, as no answer runs past its most. A
    /// price that isn't one charges everything.
    pub(super) fn cut_short(self, asked: Asked, most: u64) -> u64 {
        let Some(rates) = self.rates() else {
            return u64::MAX;
        };
        micros(&[
            (rates.input, asked.input),
            (rates.cache_write, asked.cache_write),
            (rates.cache_read, asked.cache_read),
            (rates.output, most),
        ])
    }
}

/// Dollars as whole millionths, where they are a price: finite, not
/// below nothing and within a `u64`, rounded to the nearest millionth,
/// as no price or cap is given finer. Read from the dollars written out
/// to six places, so no float is cast.
pub(super) fn millionths(dollars: f64) -> Option<u64> {
    let written = format!("{dollars:.6}");
    let (whole, part) = written.split_once('.')?;
    let digits = |text: &str| {
        text.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| text.parse::<u64>().ok())
            .flatten()
    };
    digits(whole)?
        .checked_mul(1_000_000)?
        .checked_add(digits(part)?)
}

/// Each rate, in millionths of a dollar a million tokens, times its
/// tokens, summed, in millionths of a dollar: rounded up, so a charge is
/// never under what it cost, and saturated past what a `u64` holds.
fn micros(priced: &[(u64, u64)]) -> u64 {
    let cost = priced.iter().fold(0_u128, |sum, &(rate, tokens)| {
        sum.saturating_add(u128::from(rate).saturating_mul(u128::from(tokens)))
    });
    u64::try_from(cost.div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// Millionths of a dollar as dollars and cents, for a line or a refusal,
/// the part of a cent past them left off.
pub(super) fn dollars(micros: u64) -> String {
    format!("{}.{:02}", micros / 1_000_000, micros % 1_000_000 / 10_000)
}

/// Millionths of a dollar as a number of dollars, to the millionth, for
/// a lend's line.
pub(super) fn usd(micros: u64) -> serde_json::Value {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
        .parse::<serde_json::Number>()
        .map_or(serde_json::Value::Null, serde_json::Value::Number)
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

/// A model's tokens asked, as its upstream states them before it answers
/// with any: those not from a cache, and those written to and read from
/// its cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Asked {
    input: u64,
    cache_write: u64,
    cache_read: u64,
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

    /// The tokens the answer said were asked, once it said how many came
    /// from no cache, whether or not it was done: a stream's
    /// `message_start` states them before any text, so an answer cut
    /// short can be charged them rather than its whole reservation. An
    /// upstream stating those from no cache states its cache's beside
    /// them, as a `message_start` does, or counts them within them, as a
    /// `prompt_tokens` does, so a cache's count left out is none.
    pub(super) const fn asked(&self) -> Option<Asked> {
        let Some(input) = self.usage.input else {
            return None;
        };
        Some(Asked {
            input,
            cache_write: self.usage.cache_write,
            cache_read: self.usage.cache_read,
        })
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
/// old or silent of `named`, 503 too where it has only seen `named`
/// spent and declares no such budget, and 429 where it says `named` is
/// past an allowance, as one its provider refuses or one spent.
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
    // Every entry of the name is read, so one listed twice can't lend on
    // the strength of its first.
    let standings: Vec<&Standing> = said
        .budgets
        .iter()
        .filter(|budget| budget.name == named)
        .collect();
    if standings.is_empty() {
        return Err(silent(format!("it says nothing of the budget {named}")));
    }
    // spend-watch lists a budget it has seen spent but was never told of,
    // with no allowances, so a misspelt name would otherwise be lent as
    // if nothing capped it.
    if standings.iter().any(|standing| standing.undeclared()) {
        return Err(refused(
            503,
            format!(
                "spend-watch declares no budget {named}, so no model is lent; declare it in \
                 spend-watch, or lend under a budget it declares"
            ),
        ));
    }
    match standings
        .iter()
        .flat_map(|standing| &standing.allowances)
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
    /// Whether spend-watch has only seen it spent, and was never told of
    /// it: read loosely, so a marker of another type refuses only this
    /// budget rather than making the whole word unreadable.
    #[serde(default)]
    undeclared: serde_json::Value,
    #[serde(default)]
    allowances: Vec<Allowance>,
}

impl Standing {
    /// Whether this budget is lent nothing as one spend-watch never knew:
    /// any marker but none, null or false, so one it can't read is held
    /// to be one.
    fn undeclared(&self) -> bool {
        !matches!(
            self.undeclared,
            serde_json::Value::Null | serde_json::Value::Bool(false)
        )
    }
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
