//! The diagnostics log: one line of JSON per [`Event`], appended to
//! `diagnostics.jsonl` in the data directory, the report `clanka diagnostics`
//! makes of it, and the sending of it to the endpoint its owner names in
//! `[diagnostics.upload]`, with the token `clanka diagnostics token` put
//! away. Without that setting the file never leaves the machine. An event cannot hold what was said or remembered (see
//! [`clanka_core::diagnostics`]): what is added here is when, which build,
//! which front end, which process and the end of the companion's ID.

use crate::setup::{Config, Keys, Paths};
use anyhow::{Context, Result, bail};
use clanka_core::diagnostics::{Event, Recorder};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uuid::Uuid;

/// The log starts over past this size, keeping the last full one beside it
/// (`diagnostics.1.jsonl`): about eight thousand turns each.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The front end a process is, written on each of its events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// A `clanka` command other than `chat` and `mcp`.
    Cli,
    Chat,
    Mcp,
    Hermes,
}

impl Host {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Chat => "chat",
            Self::Mcp => "mcp",
            Self::Hermes => "hermes",
        }
    }
}

static HOST: OnceLock<Host> = OnceLock::new();

/// Says which front end this process is; `cli` unless said. The first call
/// stands.
pub fn start(host: Host) {
    let _ = HOST.set(host);
}

/// Where events are appended, or nowhere when `[diagnostics] enabled` is off.
pub struct Log {
    file: Option<PathBuf>,
}

impl Log {
    pub fn open(paths: &Paths, config: &Config) -> Self {
        Self {
            file: config.diagnostics.enabled.then(|| Self::file(paths)),
        }
    }

    /// A log that keeps nothing.
    pub fn off() -> Self {
        Self { file: None }
    }

    /// A log in `file`.
    pub fn to(file: PathBuf) -> Self {
        Self { file: Some(file) }
    }

    /// Where the log is, kept or not.
    pub fn file(paths: &Paths) -> PathBuf {
        paths.data.join("diagnostics.jsonl")
    }
}

/// The last full log beside `file`.
fn rotated(file: &Path) -> PathBuf {
    file.with_extension("1.jsonl")
}

impl Recorder for Log {
    fn record(&self, companion: Option<Uuid>, event: &Event) {
        let Some(file) = &self.file else {
            return;
        };
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as i64);
        if let Some(line) = line(companion, event, at) {
            // A log that can't be written costs nothing but the line.
            let _ = append(file, &line);
        }
    }
}

fn line(companion: Option<Uuid>, event: &Event, at: i64) -> Option<String> {
    let Ok(Value::Object(mut line)) = serde_json::to_value(event) else {
        return None;
    };
    line.insert("at".into(), at.into());
    line.insert("version".into(), env!("CARGO_PKG_VERSION").into());
    let host = HOST.get().copied().unwrap_or(Host::Cli);
    line.insert("host".into(), host.as_str().into());
    line.insert("pid".into(), std::process::id().into());
    if let Some(companion) = companion {
        // The end of the ID: a UUIDv7 starts with when it was made, which two
        // companions made in the same minute share.
        let id = companion.simple().to_string();
        line.insert("companion".into(), id[id.len() - 8..].into());
    }
    Some(format!("{}\n", Value::Object(line)))
}

fn append(file: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(file).is_ok_and(|m| m.len() >= MAX_BYTES) {
        std::fs::rename(file, rotated(file))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // One write per line: several processes append to the same file.
    options.open(file)?.write_all(line.as_bytes())
}

/// What the log holds, counted.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Report {
    pub events: usize,
    /// Lines that are not events: a write cut off, or a file edited by hand.
    pub unreadable: usize,
    /// When the first and the last event were, in milliseconds since the epoch.
    pub first: Option<i64>,
    pub last: Option<i64>,
    pub companions: usize,
    /// The builds each front end ran. More than one at a time is a plugin or
    /// a CLI left behind by an upgrade.
    pub versions: BTreeMap<String, BTreeSet<String>>,
    /// Per kind of event; a memory pass and its model calls per pass, as
    /// `memory_pass.reflect`.
    pub kinds: BTreeMap<String, Summary>,
}

/// The events of one kind.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub count: usize,
    pub hosts: BTreeMap<String, usize>,
    /// Fields that hold a number: how it spreads.
    pub numbers: BTreeMap<String, Spread>,
    /// Fields that hold yes or no: how often yes.
    pub flags: BTreeMap<String, usize>,
    /// Fields that hold a word: how often each.
    pub words: BTreeMap<String, BTreeMap<String, usize>>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Spread {
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    /// How many of the events had 0 here.
    pub zero: usize,
}

#[derive(Default)]
struct Tally {
    count: usize,
    hosts: BTreeMap<String, usize>,
    numbers: BTreeMap<String, Vec<f64>>,
    flags: BTreeMap<String, usize>,
    words: BTreeMap<String, BTreeMap<String, usize>>,
}

impl Tally {
    /// Counts one field by what it holds, so a field a later build adds is
    /// counted without this one knowing it.
    fn add(&mut self, field: &str, value: &Value) {
        match value {
            Value::Null => {}
            Value::Bool(yes) => *self.flags.entry(field.into()).or_default() += usize::from(*yes),
            Value::Number(n) => self
                .numbers
                .entry(field.into())
                .or_default()
                .extend(n.as_f64()),
            Value::String(word) => {
                let words = self.words.entry(field.into()).or_default();
                *words.entry(word.clone()).or_default() += 1;
            }
            Value::Array(items) => {
                for item in items {
                    self.add(field, item);
                }
            }
            Value::Object(fields) => {
                for (name, value) in fields {
                    self.add(&format!("{field}.{name}"), value);
                }
            }
        }
    }

    fn summary(self) -> Summary {
        Summary {
            count: self.count,
            hosts: self.hosts,
            numbers: self
                .numbers
                .into_iter()
                .map(|(field, values)| (field, spread(values)))
                .collect(),
            flags: self.flags,
            words: self.words,
        }
    }
}

fn spread(mut values: Vec<f64>) -> Spread {
    values.sort_by(f64::total_cmp);
    // Nearest rank: the smallest value at least that share are at or below.
    let at = |share: f64| {
        let rank = (share * values.len() as f64).ceil() as usize;
        values
            .get(rank.saturating_sub(1))
            .copied()
            .unwrap_or_default()
    };
    Spread {
        p50: at(0.5),
        p95: at(0.95),
        max: values.last().copied().unwrap_or_default(),
        zero: values.iter().filter(|v| **v == 0.0).count(),
    }
}

/// What every line carries beside its event.
const ENVELOPE: [&str; 6] = ["at", "version", "host", "pid", "companion", "event"];

impl Report {
    /// Reads the log in `file` and the full one before it, keeping the events
    /// from `since` on (milliseconds since the epoch). A log that isn't there
    /// is an empty one.
    pub fn read(file: &Path, since: Option<i64>) -> std::io::Result<Self> {
        let mut report = Self::default();
        let mut kinds: BTreeMap<String, Tally> = BTreeMap::new();
        let mut companions = BTreeSet::new();
        for file in [rotated(file), file.to_owned()] {
            let text = match std::fs::read_to_string(&file) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                let event = serde_json::from_str::<Value>(line).ok();
                let fields = event.as_ref().and_then(Value::as_object);
                let word = |name: &str| fields?.get(name)?.as_str();
                let (Some(fields), Some(kind), Some(at)) =
                    (fields, word("event"), fields.and_then(|f| f["at"].as_i64()))
                else {
                    report.unreadable += 1;
                    continue;
                };
                if since.is_some_and(|since| at < since) {
                    continue;
                }
                report.events += 1;
                report.first = Some(report.first.map_or(at, |first| first.min(at)));
                report.last = Some(report.last.map_or(at, |last| last.max(at)));
                companions.extend(word("companion").map(str::to_owned));
                let host = word("host").unwrap_or("?");
                if let Some(version) = word("version") {
                    let versions = report.versions.entry(host.into()).or_default();
                    versions.insert(version.into());
                }
                let kind = match word("pass") {
                    Some(pass) => format!("{kind}.{pass}"),
                    None => kind.to_owned(),
                };
                let tally = kinds.entry(kind).or_default();
                tally.count += 1;
                *tally.hosts.entry(host.into()).or_default() += 1;
                for (name, value) in fields {
                    if !ENVELOPE.contains(&name.as_str()) && name != "pass" {
                        tally.add(name, value);
                    }
                }
            }
        }
        report.companions = companions.len();
        report.kinds = kinds
            .into_iter()
            .map(|(kind, tally)| (kind, tally.summary()))
            .collect();
        Ok(report)
    }

    /// The report as lines to read.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let (Some(first), Some(last)) = (self.first, self.last) else {
            out.push_str("No events.\n");
            return out;
        };
        let day = clanka_core::context::date;
        let _ = writeln!(
            out,
            "{} events from {} to {}, {} companion{}.",
            self.events,
            day(first),
            day(last),
            self.companions,
            if self.companions == 1 { "" } else { "s" },
        );
        if self.unreadable > 0 {
            let _ = writeln!(out, "{} lines are not events.", self.unreadable);
        }
        let versions: Vec<String> = self
            .versions
            .iter()
            .map(|(host, versions)| {
                let versions: Vec<&str> = versions.iter().map(String::as_str).collect();
                format!("{host} {}", versions.join(" and "))
            })
            .collect();
        let _ = writeln!(out, "Builds: {}.", versions.join(", "));
        for (kind, summary) in &self.kinds {
            let _ = writeln!(
                out,
                "\n{kind}: {} ({})",
                summary.count,
                counted(&summary.hosts)
            );
            for (field, s) in &summary.numbers {
                let _ = write!(
                    out,
                    "  {field:<16} p50 {}, p95 {}, max {}",
                    s.p50, s.p95, s.max
                );
                let _ = match s.zero {
                    0 => writeln!(out),
                    zero => writeln!(out, ", 0 in {zero}"),
                };
            }
            for (field, yes) in &summary.flags {
                let _ = writeln!(out, "  {field:<16} {yes} of {}", summary.count);
            }
            for (field, words) in &summary.words {
                let _ = writeln!(out, "  {field:<16} {}", counted(words));
            }
        }
        out
    }
}

/// `name count`, the most frequent first.
fn counted(counts: &BTreeMap<String, usize>) -> String {
    let mut counts: Vec<_> = counts.iter().collect();
    counts.sort_by(|a, b| b.1.cmp(a.1));
    let counts: Vec<String> = counts
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect();
    counts.join(", ")
}

/// Events one request carries, at most.
const BATCH_EVENTS: usize = 500;

/// Longest one request of a send may take.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Longest the whole send at the end of a conversation may take, where
/// someone may be waiting for the prompt and a reflection for its turn. A log
/// with more to send than fits goes on with the next conversation.
pub const QUIET_SEND: Duration = Duration::from_secs(4);

/// Where a send goes, and what it carries to be let in. Not `Debug`: the
/// token is not for printing.
pub struct Upload {
    /// Takes a POST of one JSON object per line.
    pub url: String,
    /// Sent as `Authorization: Bearer`, when the endpoint asks for one.
    pub token: Option<String>,
}

impl Upload {
    /// Where the owner asked for the log to be sent: the endpoint of
    /// `[diagnostics.upload]` and the token kept with the keys. `None`
    /// without that setting, and then the key store is not asked.
    pub fn load(paths: &Paths, config: &Config) -> Result<Option<Self>> {
        let Some(upload) = &config.diagnostics.upload else {
            return Ok(None);
        };
        Ok(Some(Self {
            url: upload.url.clone(),
            token: Keys::new(paths, config).upload_token()?,
        }))
    }

    /// As [`Self::load`], for when nobody may be there to answer the key
    /// store: an error once it has kept the token back for `within`. The
    /// login keychain asks its owner before it gives an item to a program
    /// that did not store it, such as the CLI after an upgrade or Hermes,
    /// and waits for the answer for as long as it takes.
    fn load_within(paths: &Paths, config: &Config, within: Duration) -> Result<Option<Self>> {
        // One question at a time: a second one, while the first waits on its
        // owner, would only put another prompt behind it.
        static ASKING: AtomicBool = AtomicBool::new(false);
        let Some(upload) = &config.diagnostics.upload else {
            return Ok(None);
        };
        let waiting = "the key store has not handed over the upload token: it may be asking \
                       whether clanka may use it (choose Always Allow). The events go with a \
                       later send";
        if ASKING.swap(true, Ordering::SeqCst) {
            bail!("{waiting}");
        }
        let keys = Keys::new(paths, config);
        let token = answer_within(within, move || {
            let token = keys.upload_token();
            ASKING.store(false, Ordering::SeqCst);
            token
        });
        match token {
            Some(token) => Ok(Some(Self {
                url: upload.url.clone(),
                token: token?,
            })),
            None => bail!("{waiting}"),
        }
    }
}

/// What `ask` answers, or `None` when it has not within `within`. It is then
/// left to finish on its own, and its answer is dropped.
fn answer_within<T: Send + 'static>(
    within: Duration,
    ask: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = answer.send(ask());
    });
    answered.recv_timeout(within).ok()
}

/// What an endpoint keeps of a token to know it again: its SHA-256, as
/// lowercase hex. It says nothing else about the token, so it can be shown.
pub fn token_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(token.as_bytes());
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a send did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sent {
    pub events: usize,
    pub requests: usize,
}

/// How far the log has been sent: `offset` bytes of the file whose first line
/// hashes to `head`. The file is named by its first line, not its path: the
/// log starts over when it is full, and the one being sent is then the one
/// beside it.
struct Cursor {
    head: String,
    offset: u64,
}

fn cursor_file(file: &Path) -> PathBuf {
    file.with_extension("sent")
}

fn cursor(file: &Path) -> Option<Cursor> {
    let text = std::fs::read_to_string(cursor_file(file)).ok()?;
    let (offset, head) = text.trim().split_once(' ')?;
    Some(Cursor {
        head: head.to_owned(),
        offset: offset.parse().ok()?,
    })
}

fn set_cursor(file: &Path, head: &str, offset: u64) -> std::io::Result<()> {
    // Written whole and renamed: a cursor cut off would send the log again.
    let partial = file.with_extension("sent.part");
    std::fs::write(&partial, format!("{offset} {head}\n"))?;
    std::fs::rename(partial, cursor_file(file))
}

/// The hash of a log's first line, or `None` for one with no whole line.
fn head(file: &Path) -> std::io::Result<Option<String>> {
    use sha2::{Digest, Sha256};
    use std::io::BufRead;
    let log = match std::fs::File::open(file) {
        Ok(log) => log,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut line = Vec::new();
    std::io::BufReader::new(log).read_until(b'\n', &mut line)?;
    if !line.ends_with(b"\n") {
        return Ok(None);
    }
    let hash = Sha256::digest(&line);
    Ok(Some(hash.iter().map(|b| format!("{b:02x}")).collect()))
}

/// The parts of the log not sent yet, oldest first: each file and the byte
/// its unsent lines start at.
fn pending(file: &Path) -> std::io::Result<Vec<(PathBuf, u64)>> {
    let before = rotated(file);
    if let Some(cursor) = cursor(file) {
        if head(file)?.as_deref() == Some(&cursor.head) {
            return Ok(vec![(file.to_owned(), cursor.offset)]);
        }
        if head(&before)?.as_deref() == Some(&cursor.head) {
            return Ok(vec![(before, cursor.offset), (file.to_owned(), 0)]);
        }
    }
    // Nothing was sent yet, or the log sent from is gone: it started over
    // twice since. Both files are newer than anything sent.
    Ok(vec![(before, 0), (file.to_owned(), 0)])
}

/// The whole lines of `file` from byte `start` on, and where they start: at
/// the end of a file shorter than `start`.
fn tail(file: &Path, start: u64) -> std::io::Result<(u64, Vec<u8>)> {
    let mut log = match std::fs::File::open(file) {
        Ok(log) => log,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, Vec::new())),
        Err(e) => return Err(e),
    };
    let start = start.min(log.metadata()?.len());
    log.seek(std::io::SeekFrom::Start(start))?;
    let mut lines = Vec::new();
    log.read_to_end(&mut lines)?;
    // A line still being written is the next send's.
    let whole = lines.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    lines.truncate(whole);
    Ok((start, lines))
}

/// How many events of the log are not sent yet.
pub fn unsent(file: &Path) -> std::io::Result<usize> {
    let mut events = 0;
    for (part, start) in pending(file)? {
        let (_, lines) = tail(&part, start)?;
        events += lines
            .split(|b| *b == b'\n')
            .filter(|line| serde_json::from_slice::<serde_json::Map<String, Value>>(line).is_ok())
            .count();
    }
    Ok(events)
}

/// This machine's name to whoever receives its log: random, made on the
/// first send and kept beside the log, so one install's events can be told
/// from another's. It says nothing else about the machine.
fn install(file: &Path) -> std::io::Result<String> {
    let path = file.with_extension("id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(id.to_owned());
        }
    }
    let id = crate::setup::random_hex(16);
    let dir = file.parent().unwrap_or(Path::new("."));
    crate::setup::write_private(dir, &path, &id)?;
    Ok(id)
}

/// A line of the log as it is sent: with the install, the system and the
/// processor. `None` for a line that is not an event.
fn outgoing(line: &[u8], install: &str) -> Option<String> {
    let mut event: serde_json::Map<String, Value> = serde_json::from_slice(line).ok()?;
    event.insert("install".into(), install.into());
    event.insert("os".into(), std::env::consts::OS.into());
    event.insert("arch".into(), std::env::consts::ARCH.into());
    Some(Value::Object(event).to_string())
}

/// Sends the events of the log in `file` that were not sent yet to `upload`,
/// oldest first: one JSON object per line (NDJSON) in the body of a POST, at
/// most [`BATCH_EVENTS`] per request, with the token as a bearer. A request
/// the endpoint doesn't answer with a 2xx stops the send; what it carried, and
/// everything after it, goes with the next one. An event is sent at least
/// once: twice when the answer to its request was lost, or when two processes
/// send at the same moment.
///
/// Past `within`, when there is one, the send stops between requests and
/// leaves the rest for the next.
pub fn send(file: &Path, upload: &Upload, within: Option<Duration>) -> Result<Sent> {
    let install = install(file)?;
    let started = std::time::Instant::now();
    let left = || within.map(|within| within.saturating_sub(started.elapsed()));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let post = |body: &str| -> Result<()> {
        let timeout = left().map_or(SEND_TIMEOUT, |left| left.min(SEND_TIMEOUT));
        let mut request = agent
            .post(&upload.url)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .header("Content-Type", "application/x-ndjson")
            .header("User-Agent", concat!("clanka/", env!("CARGO_PKG_VERSION")));
        if let Some(token) = &upload.token {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        let mut response = request.send(body).with_context(|| upload.url.clone())?;
        let status = response.status();
        if !status.is_success() {
            let said = response
                .body_mut()
                .with_config()
                .limit(4096)
                .read_to_string()
                .unwrap_or_default();
            let said: String = said.trim().chars().take(200).collect();
            bail!("{}: HTTP {status}: {said}", upload.url);
        }
        Ok(())
    };
    let mut sent = Sent::default();
    for (part, start) in pending(file)? {
        let Some(head) = head(&part)? else {
            continue;
        };
        let (start, lines) = tail(&part, start)?;
        let (mut offset, mut batch, mut events) = (start, String::new(), 0);
        let mut lines = lines.split_inclusive(|b| *b == b'\n').peekable();
        while let Some(line) = lines.next() {
            offset += line.len() as u64;
            if let Some(event) = outgoing(line, &install) {
                batch.push_str(&event);
                batch.push('\n');
                events += 1;
            }
            if events == BATCH_EVENTS || lines.peek().is_none() {
                if events > 0 {
                    if left().is_some_and(|left| left.is_zero()) {
                        return Ok(sent);
                    }
                    post(&batch)?;
                    sent.events += events;
                    sent.requests += 1;
                }
                set_cursor(file, &head, offset)?;
                (batch, events) = (String::new(), 0);
            }
        }
    }
    Ok(sent)
}

/// The send at the end of a conversation, for an owner who asked for the log
/// to be sent: nothing without `[diagnostics.upload]`, or with the log off.
/// [`QUIET_SEND`] covers the wait for the token too.
pub fn send_after_conversation(paths: &Paths, config: &Config) -> Result<Sent> {
    if !config.diagnostics.enabled {
        return Ok(Sent::default());
    }
    let started = std::time::Instant::now();
    match Upload::load_within(paths, config, QUIET_SEND)? {
        Some(upload) => {
            let left = QUIET_SEND.saturating_sub(started.elapsed());
            send(&Log::file(paths), &upload, Some(left))
        }
        None => Ok(Sent::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clanka_core::diagnostics::{Pass, Refused, Tics};

    #[test]
    fn the_report_counts_what_the_log_holds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data/diagnostics.jsonl");
        let log = Log::to(file.clone());
        let nova = Uuid::from_u128(0x0193_a1b2_0000_7000_8000_0000_cafe_f00d);
        let reply = |reply_chars, negation| Event::TurnEnd {
            ms: 2,
            reply_chars,
            dashes: negation > 0,
            tics: Tics {
                negation,
                ..Tics::default()
            },
            recalled: 0,
            error: None,
        };
        log.record(Some(nova), &reply(120, 0));
        log.record(Some(nova), &reply(0, 2));
        log.record(
            Some(nova),
            &Event::MemoryPass {
                pass: Pass::Reflect,
                ms: 900,
                late: false,
                rounds: 2,
                ok: 1,
                refused: vec![
                    Refused {
                        tool: "remember",
                        code: "duplicate",
                    };
                    2
                ],
                left_out: 0,
                error: Some("llm"),
            },
        );
        log.record(
            None,
            &Event::Plugin {
                what: "open_timeout",
            },
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"{\"cut off\n")
            .unwrap();

        let report = Report::read(&file, None).unwrap();
        assert_eq!((report.events, report.unreadable), (4, 1));
        assert_eq!(report.companions, 1);
        let build: BTreeSet<String> = [env!("CARGO_PKG_VERSION").to_owned()].into();
        assert_eq!(report.versions, BTreeMap::from([("cli".to_owned(), build)]));

        let replies = &report.kinds["turn_end"];
        assert_eq!(replies.count, 2);
        // One of the two replies was empty.
        assert_eq!(
            replies.numbers["reply_chars"],
            Spread {
                p50: 0.0,
                p95: 120.0,
                max: 120.0,
                zero: 1
            }
        );
        assert_eq!(replies.numbers["tics.negation"].max, 2.0);
        assert_eq!(replies.flags["dashes"], 1);

        // A pass is counted per pass, with why its calls were refused.
        let reflections = &report.kinds["memory_pass.reflect"];
        assert_eq!(reflections.words["refused"]["remember/duplicate"], 2);
        assert_eq!(reflections.words["error"]["llm"], 1);
        assert_eq!(report.kinds["plugin"].words["what"]["open_timeout"], 1);

        let text = report.text();
        assert!(text.contains("4 events from "), "{text}");
        assert!(
            text.contains("refused          remember/duplicate 2"),
            "{text}"
        );
        assert!(
            text.contains("reply_chars      p50 0, p95 120, max 120, 0 in 1"),
            "{text}"
        );

        // Nothing from before `since`, and a log that isn't there is empty.
        let later = Report::read(&file, Some(i64::MAX)).unwrap();
        assert_eq!((later.events, later.unreadable), (0, 1));
        let none = Report::read(&dir.path().join("none.jsonl"), None).unwrap();
        assert_eq!(none.text(), "No events.\n");
    }

    #[test]
    fn a_full_log_starts_over_and_is_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("diagnostics.jsonl");
        let event = Event::Plugin {
            what: "turn_blocked",
        };
        // One byte short of full: the next line still goes in, and the one
        // after it starts the log over.
        std::fs::write(&file, " ".repeat(MAX_BYTES as usize - 1)).unwrap();
        let log = Log::to(file.clone());
        log.record(None, &event);
        assert!(!rotated(&file).exists());
        log.record(None, &event);
        assert!(std::fs::metadata(rotated(&file)).unwrap().len() > MAX_BYTES);
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
        assert_eq!(Report::read(&file, None).unwrap().events, 2);
    }

    /// An endpoint on localhost that answers each request with the next
    /// status, then stops. Returns its URL and, per request, its
    /// `Authorization` header and its body.
    fn endpoint(statuses: Vec<u16>) -> (String, std::sync::mpsc::Receiver<(String, String)>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/events", listener.local_addr().unwrap());
        let (got, requests) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                let (head, length) = loop {
                    let n = stream.read(&mut buf).unwrap();
                    request.extend_from_slice(&buf[..n]);
                    let Some(head) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let length: usize = String::from_utf8_lossy(&request[..head])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if n == 0 || request.len() >= head + 4 + length {
                        break (head, length);
                    }
                };
                let headers = String::from_utf8_lossy(&request[..head]).into_owned();
                let token = headers
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("authorization: ")
                            .or(l.strip_prefix("Authorization: "))
                    })
                    .unwrap_or_default()
                    .to_owned();
                let body = String::from_utf8_lossy(&request[head + 4..head + 4 + length]);
                got.send((token, body.into_owned())).unwrap();
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno"
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        (url, requests)
    }

    fn upload(url: String) -> Upload {
        Upload {
            url,
            token: Some("s3cret".into()),
        }
    }

    #[test]
    fn a_key_store_that_keeps_its_answer_back_is_not_waited_for() {
        assert_eq!(answer_within(Duration::from_secs(5), || 7), Some(7));
        // A prompt nobody answers: the question never returns.
        let (never, asked) = std::sync::mpsc::channel::<()>();
        let started = std::time::Instant::now();
        let answer = answer_within(Duration::from_millis(50), move || asked.recv());
        assert!(answer.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(never);
    }

    #[test]
    fn a_tokens_hash_is_the_one_an_endpoint_computes() {
        // SHA-256 of the token's bytes: the test vector of FIPS 180-2.
        assert_eq!(
            token_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn blocked() -> Event {
        Event::Plugin {
            what: "turn_blocked",
        }
    }

    #[test]
    fn each_event_is_sent_once_with_who_sent_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("diagnostics.jsonl");
        let log = Log::to(file.clone());
        for _ in 0..BATCH_EVENTS + 1 {
            log.record(None, &blocked());
        }
        assert_eq!(unsent(&file).unwrap(), BATCH_EVENTS + 1);

        // More than one request holds goes in two; nothing is left after.
        let (url, requests) = endpoint(vec![200, 204, 200]);
        let upload = upload(url);
        let sent = send(&file, &upload, None).unwrap();
        assert_eq!((sent.events, sent.requests), (BATCH_EVENTS + 1, 2));
        let (token, first) = requests.recv().unwrap();
        assert_eq!(token, "Bearer s3cret");
        assert_eq!(first.lines().count(), BATCH_EVENTS);
        let (_, second) = requests.recv().unwrap();
        assert_eq!(second.lines().count(), 1);
        assert_eq!(unsent(&file).unwrap(), 0);
        assert_eq!(send(&file, &upload, None).unwrap(), Sent::default());

        // A line is the event as logged, with the install and the system.
        let line: Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
        assert_eq!(line["event"], "plugin");
        assert_eq!(line["what"], "turn_blocked");
        assert_eq!(line["os"], std::env::consts::OS);
        assert_eq!(line["arch"], std::env::consts::ARCH);
        let install = line["install"].as_str().unwrap().to_owned();
        assert_eq!(install.len(), 32);

        // Only what was logged since goes next, under the same install.
        log.record(
            None,
            &Event::Plugin {
                what: "open_timeout",
            },
        );
        let sent = send(&file, &upload, None).unwrap();
        assert_eq!((sent.events, sent.requests), (1, 1));
        let (_, third) = requests.recv().unwrap();
        let line: Value = serde_json::from_str(third.trim()).unwrap();
        assert_eq!(line["what"], "open_timeout");
        assert_eq!(line["install"], install.as_str());
    }

    #[test]
    fn a_refused_send_goes_again_and_a_log_that_started_over_is_sent_whole() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("diagnostics.jsonl");
        let log = Log::to(file.clone());
        log.record(None, &blocked());
        log.record(None, &blocked());

        let (url, requests) = endpoint(vec![401, 200, 200, 200]);
        let upload = upload(url);
        let refused = send(&file, &upload, None).unwrap_err().to_string();
        assert!(refused.contains("HTTP 401"), "{refused}");
        assert!(!refused.contains("s3cret"), "{refused}");
        let (_, refused) = requests.recv().unwrap();
        assert_eq!(unsent(&file).unwrap(), 2);
        assert_eq!(send(&file, &upload, None).unwrap().events, 2);
        assert_eq!(requests.recv().unwrap().1, refused);

        // One more event, then the log fills and starts over with another:
        // the end of the old log and the new one both go, once.
        log.record(
            None,
            &Event::Plugin {
                what: "open_timeout",
            },
        );
        std::fs::rename(&file, rotated(&file)).unwrap();
        log.record(
            None,
            &Event::Plugin {
                what: "goodnight_not_sent",
            },
        );
        assert_eq!(unsent(&file).unwrap(), 2);
        let sent = send(&file, &upload, None).unwrap();
        assert_eq!((sent.events, sent.requests), (2, 2));
        let old = requests.recv().unwrap().1;
        let new = requests.recv().unwrap().1;
        assert!(
            old.contains("open_timeout") && old.lines().count() == 1,
            "{old}"
        );
        assert!(
            new.contains("goodnight_not_sent") && new.lines().count() == 1,
            "{new}"
        );
        assert_eq!(unsent(&file).unwrap(), 0);
    }

    #[test]
    fn nothing_is_sent_at_a_conversations_end_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: dir.path().join("config"),
            souls: dir.path().join("config/souls"),
            data: dir.path().join("data"),
            models: dir.path().join("data/models"),
        };
        Log::open(&paths, &Config::default()).record(None, &blocked());
        // No endpoint is named, so none is asked: a send would fail on this one.
        assert_eq!(
            send_after_conversation(&paths, &Config::default()).unwrap(),
            Sent::default()
        );
        // No install is made up for a machine that sends nothing.
        assert!(!Log::file(&paths).with_extension("id").exists());

        // Keys in files, so the test never asks the machine's Secret Service.
        let (url, requests) = endpoint(vec![200, 200]);
        let asked: Config = toml::from_str(&format!(
            "[keys]\nstore = \"file\"\n[diagnostics.upload]\nurl = \"{url}\"\n"
        ))
        .unwrap();
        assert_eq!(send_after_conversation(&paths, &asked).unwrap().events, 1);
        // No token put away, no header.
        assert_eq!(requests.recv().unwrap().0, "");

        // The token put away with the keys goes with the next send.
        Keys::new(&paths, &asked)
            .store_upload_token("s3cret")
            .unwrap();
        Log::open(&paths, &asked).record(None, &blocked());
        assert_eq!(send_after_conversation(&paths, &asked).unwrap().events, 1);
        assert_eq!(requests.recv().unwrap().0, "Bearer s3cret");
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_private_and_off_keeps_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("diagnostics.jsonl");
        Log::to(file.clone()).record(
            None,
            &Event::Plugin {
                what: "turn_blocked",
            },
        );
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let paths = Paths {
            config: dir.path().join("config"),
            souls: dir.path().join("config/souls"),
            data: dir.path().join("off"),
            models: dir.path().join("off/models"),
        };
        let config: Config = toml::from_str("[diagnostics]\nenabled = false\n").unwrap();
        Log::open(&paths, &config).record(
            None,
            &Event::Plugin {
                what: "turn_blocked",
            },
        );
        assert!(!Log::file(&paths).exists());
        Log::open(&paths, &Config::default()).record(
            None,
            &Event::Plugin {
                what: "turn_blocked",
            },
        );
        assert!(Log::file(&paths).exists());
    }
}
