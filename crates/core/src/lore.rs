//! Lore: shared world reference that companions open while they talk.
//!
//! An entry is a file in the lore directory, `<id>.md`: TOML frontmatter
//! between `+++` lines (`title`, `aliases`, `opens`, `sealed`), then the
//! user's Markdown, which clanka never writes on its own. Beside it,
//! `<id>.ledger.jsonl` holds what companions added, one JSON line each:
//! deeds, notes they released, digests, and the user's strikes. A ledger
//! with no `.md` is a stub a companion made for a place the user named.
//!
//! Every line's provenance is computed here, against the user's words, never
//! taken from the model: a note is `told` when every name and number in it,
//! and most of its other words, were said by the user or are in the entry,
//! `worked_out` otherwise, `dreamt` from a dream; a deed quotes the exchange
//! and is `narrated` when the quote is the user's words, `claimed` when only
//! the companion said it.
//!
//! Which entries a companion reads is its soul's `lore` grants, plus what an
//! entry it read `opens`. Its private notes, what is open, its read receipts
//! and lines not yet appended live in its database.

use crate::context::estimate_tokens;
use crate::deps::{ToolCall, ToolSpec};
use crate::diagnostics::Pass;
use crate::soul::{Dreams, sha256_hex};
use crate::writer::{args, said_verbatim};
use crate::{Companion, Error, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use uuid::Uuid;

/// The lore's allotment per turn, in estimated tokens: apart from the memory
/// budget and the conversation's allowance, so it never tires a companion.
pub const LORE_TOKENS: usize = 3_000;

/// Characters in a note or a deed.
pub const LINE_CHARS: usize = 500;

/// Characters in a digest.
pub const DIGEST_CHARS: usize = 800;

/// Characters in a stub's title.
pub const TITLE_CHARS: usize = 80;

/// Words a deed must quote from the exchange.
pub const DEED_WORDS: usize = 3;

/// Lore calls one reflection or waking may make, apart from its memory calls.
pub const LORE_CALLS: usize = 2;

/// Lines after the last digest that make the keeper fold an entry.
pub const KEEPER_AFTER: usize = 20;

/// Entries the index lists.
const INDEX_LIMIT: usize = 40;

/// Companions' lines shown under an open entry, strongest first.
const SHOWN_LINES: usize = 12;

/// Private notes shown under an open entry, newest first.
const SHOWN_NOTES: usize = 8;

/// The names of the tools a reflection gets while an entry is open.
pub const LORE_TOOLS: [&str; 4] = ["lore_note", "lore_deed", "lore_retract", "lore_stub"];

/// Where the lore lives and how much of it a turn shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Lore {
    pub dir: PathBuf,
    /// The allotment per turn, in estimated tokens.
    pub tokens: usize,
}

/// An entry's frontmatter.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Front {
    pub title: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Entries a companion that read this one may go on to open.
    #[serde(default)]
    pub opens: Vec<String>,
    /// No machine-written lines at all: companions keep private notes only.
    #[serde(default)]
    pub sealed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub front: Front,
    /// The user's text, without the frontmatter. Empty for a stub.
    pub text: String,
    /// A ledger with no `.md`: a companion named it, nobody wrote it yet.
    pub stub: bool,
    /// The ledger, oldest first, each line once.
    pub lines: Vec<Line>,
    /// Ledger lines that aren't valid JSON lines, skipped.
    pub bad_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Deed,
    Note,
    Digest,
    Strike,
    Retract,
    Stub,
}

/// How a line's writer knows it, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// A deed the user's own words narrated.
    Narrated,
    /// Every name and number in it was the user's, or the entry's.
    Told,
    /// A deed only the companion's reply says happened.
    Claimed,
    /// Its own conclusion.
    WorkedOut,
    /// From a dream.
    Dreamt,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Narrated => "narrated",
            Provenance::Told => "told",
            Provenance::Claimed => "claimed",
            Provenance::WorkedOut => "worked_out",
            Provenance::Dreamt => "dreamt",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(json!(s)).ok()
    }

    /// How it reads under an entry.
    fn label(self) -> &'static str {
        match self {
            Provenance::Narrated => "seen",
            Provenance::Told => "told",
            Provenance::Claimed => "claimed",
            Provenance::WorkedOut => "worked out",
            Provenance::Dreamt => "dreamt",
        }
    }
}

/// A digest's claim that a deed makes a sentence of the user's text untrue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub line: Uuid,
    pub quote: String,
}

/// One line of a ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub id: Uuid,
    pub kind: Kind,
    /// The companion's ID, or `user`.
    pub by: String,
    /// The companion's name when it wrote the line.
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Milliseconds since the Unix epoch.
    pub at: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// A line of the same writer's that this one replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<Uuid>,
    /// The line a strike or a retraction is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<Uuid>,
    /// The lines a digest folds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cites: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<Anchor>,
}

impl Line {
    pub fn new(kind: Kind, by: &str, name: &str, at: i64) -> Self {
        let secs = u64::try_from(at.div_euclid(1000)).unwrap_or(0);
        let nanos = u32::try_from(at.rem_euclid(1000) * 1_000_000).unwrap_or(0);
        Line {
            id: Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, nanos)),
            kind,
            by: by.to_owned(),
            name: name.to_owned(),
            provenance: None,
            at,
            text: String::new(),
            replaces: None,
            line: None,
            cites: Vec::new(),
            anchors: Vec::new(),
        }
    }

    /// The short reference a model or a person names the line by: the last
    /// eight hex digits, the random part of its ID.
    pub fn short(&self) -> String {
        short(self.id)
    }
}

pub fn short(id: Uuid) -> String {
    let simple = id.simple().to_string();
    simple[simple.len() - 8..].to_owned()
}

/// Whether `id` is an entry ID: path segments of `[a-z0-9-]`, separated by `/`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// Whether a soul's grant (an ID, or a directory with a trailing `/`) is well formed.
pub fn valid_grant(grant: &str) -> bool {
    valid_id(grant.strip_suffix('/').unwrap_or(grant))
}

/// Whether `grant` covers the entry `id`.
pub fn grants(grant: &str, id: &str) -> bool {
    match grant.strip_suffix('/') {
        Some(dir) => id
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/')),
        None => grant == id,
    }
}

/// Splits an entry's file into its frontmatter and the user's text.
pub fn parse_entry(text: &str) -> Result<(Front, String)> {
    let unix = text.replace("\r\n", "\n");
    let rest = unix
        .strip_prefix("+++\n")
        .ok_or_else(|| Error::Lore("an entry must start with a `+++` frontmatter line".into()))?;
    let (front, body) = rest
        .split_once("\n+++\n")
        .or_else(|| rest.strip_suffix("\n+++").map(|f| (f, "")))
        .ok_or_else(|| Error::Lore("the frontmatter has no closing `+++` line".into()))?;
    let front: Front =
        toml::from_str(front).map_err(|e| Error::Lore(format!("frontmatter: {e}")))?;
    if front.title.trim().is_empty() {
        return Err(Error::Lore("the frontmatter's title is empty".into()));
    }
    if let Some(bad) = front.opens.iter().find(|id| !valid_id(id)) {
        return Err(Error::Lore(format!(
            "opens names {bad:?}, which is not an entry ID (segments of a-z, 0-9 and -)"
        )));
    }
    Ok((front, body.trim().to_owned()))
}

/// One of [`Lore::problems`]: what is wrong, and the entries it is about.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub entries: Vec<String>,
    pub text: String,
}

impl Lore {
    pub fn new(dir: PathBuf) -> Self {
        Lore {
            dir,
            tokens: LORE_TOKENS,
        }
    }

    pub fn entry_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.md"))
    }

    pub fn ledger_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.ledger.jsonl"))
    }

    /// Every entry ID in the directory, stubs included, sorted. Files whose
    /// name is no valid ID are left out (`clanka lore lint` names them).
    pub fn ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for path in self.files()? {
            if let Some(id) = self.id_of(&path)
                && valid_id(&id)
            {
                ids.push(id);
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// What keeps the lore from being read as written: a file whose name is
    /// no entry ID, an entry that doesn't parse, a ledger line that isn't one,
    /// and a title or alias several entries go by. `clanka lore lint` calls
    /// these errors, and a companion that reads them refuses to talk.
    pub fn problems(&self) -> Result<Vec<Problem>> {
        let mut out = Vec::new();
        for path in self.files()? {
            match self.id_of(&path) {
                Some(id) if valid_id(&id) => {}
                id => out.push(Problem {
                    // The entry it was meant to be, for who reads it.
                    entries: id.map(|id| id.to_lowercase()).into_iter().collect(),
                    text: format!(
                        "{}: the name is no entry ID (segments of a-z, 0-9 and -)",
                        path.display()
                    ),
                }),
            }
        }
        // Each name, as a message matches it, and the entries that go by it.
        let mut names: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for id in self.ids()? {
            match self.entry(&id) {
                Err(e) => out.push(Problem {
                    entries: vec![id],
                    text: e.to_string(),
                }),
                Ok(None) => {}
                Ok(Some(entry)) => {
                    if entry.bad_lines > 0 {
                        out.push(Problem {
                            entries: vec![id.clone()],
                            text: format!(
                                "{}: {} lines are not ledger lines and are skipped",
                                self.ledger_path(&id).display(),
                                entry.bad_lines
                            ),
                        });
                    }
                    for name in entry.names() {
                        let named = names.entry(name.trim().to_lowercase()).or_default();
                        if !named.contains(&id) {
                            named.push(id.clone());
                        }
                    }
                }
            }
        }
        // One problem per group of entries, with every name they share.
        let mut shared: std::collections::BTreeMap<Vec<String>, Vec<String>> = Default::default();
        for (name, named) in names.into_iter().filter(|(_, named)| named.len() > 1) {
            shared.entry(named).or_default().push(format!("{name:?}"));
        }
        for (entries, names) in shared {
            out.push(Problem {
                text: format!(
                    "{} all go by {}: a message that says one opens every one of them; give \
                     each name to one entry",
                    entries.join(", "),
                    names.join(", ")
                ),
                entries,
            });
        }
        Ok(out)
    }

    /// Every `.md` and `.ledger.jsonl` under the directory, outside hidden
    /// directories (`.git`).
    pub fn files(&self) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        let mut dirs = vec![self.dir.clone()];
        while let Some(dir) = dirs.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(Error::Io { path: dir, source }),
            };
            for entry in entries {
                let path = entry
                    .map_err(|source| Error::Io {
                        path: dir.clone(),
                        source,
                    })?
                    .path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('.') {
                    continue;
                }
                if path.is_dir() {
                    dirs.push(path);
                } else if name.ends_with(".md") || name.ends_with(".ledger.jsonl") {
                    out.push(path);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// The entry ID a file in the directory belongs to.
    pub fn id_of(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.dir).ok()?.to_str()?;
        let rel = rel.replace(std::path::MAIN_SEPARATOR, "/");
        rel.strip_suffix(".ledger.jsonl")
            .or_else(|| rel.strip_suffix(".md"))
            .map(str::to_owned)
    }

    /// An entry with its ledger, or `None` when there is neither file.
    pub fn entry(&self, id: &str) -> Result<Option<Entry>> {
        if !valid_id(id) {
            return Ok(None);
        }
        let md = self.entry_path(id);
        let (lines, bad_lines) = self.ledger(id)?;
        let (front, text, stub) = match std::fs::read_to_string(&md) {
            Ok(file) => {
                let (front, text) = parse_entry(&file)
                    .map_err(|e| Error::Lore(format!("{}: {e}", md.display())))?;
                (front, text, false)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let Some(stub) = lines.iter().find(|l| l.kind == Kind::Stub) else {
                    return Ok(None);
                };
                let front = Front {
                    title: stub.text.clone(),
                    ..Front::default()
                };
                (front, String::new(), true)
            }
            Err(source) => return Err(Error::Io { path: md, source }),
        };
        Ok(Some(Entry {
            id: id.to_owned(),
            front,
            text,
            stub,
            lines,
            bad_lines,
        }))
    }

    /// The ledger's lines, oldest first, each ID once (two machines' ledgers
    /// merged by git hold the union), and how many lines were not readable.
    pub fn ledger(&self, id: &str) -> Result<(Vec<Line>, usize)> {
        let path = self.ledger_path(id);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
            Err(source) => return Err(Error::Io { path, source }),
        };
        let mut seen = HashSet::new();
        let mut lines = Vec::new();
        let mut bad = 0;
        for raw in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<Line>(raw) {
                Ok(line) if seen.insert(line.id) => lines.push(line),
                Ok(_) => {}
                Err(_) => bad += 1,
            }
        }
        // Stable: lines of the same moment keep the order they were appended in.
        lines.sort_by_key(|l| l.at);
        Ok((lines, bad))
    }

    /// Appends lines to an entry's ledger under a lock held only for the
    /// append, then commits the ledger when the directory is a git
    /// repository. `author` is the companion's name and ID; `None` is the user.
    pub fn append(&self, id: &str, lines: &[Line], author: Option<(&str, Uuid)>) -> Result<()> {
        if !valid_id(id) {
            return Err(Error::Lore(format!("{id:?} is not an entry ID")));
        }
        if lines.is_empty() {
            return Ok(());
        }
        let path = self.ledger_path(id);
        let io = |source| Error::Io {
            path: path.clone(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(io)?;
        file.lock().map_err(io)?;
        let mut out = String::new();
        // A hand-edited ledger may end without its newline.
        if file.metadata().map_err(io)?.len() > 0 {
            let mut last = [0u8];
            file.seek(SeekFrom::End(-1)).map_err(io)?;
            file.read_exact(&mut last).map_err(io)?;
            if last[0] != b'\n' {
                out.push('\n');
            }
        }
        for line in lines {
            out.push_str(&serde_json::to_string(line).expect("a line serializes"));
            out.push('\n');
        }
        file.write_all(out.as_bytes()).map_err(io)?;
        file.sync_data().map_err(io)?;
        drop(file);
        let what = match lines.first().map(|l| l.kind) {
            Some(Kind::Strike) => "strike",
            Some(Kind::Digest) => "digest",
            Some(Kind::Stub) => "stub",
            _ => "lines",
        };
        self.commit(&[path], &format!("{id}: {what}"), author);
        Ok(())
    }

    /// Writes an entry's file whole, by rename, after checking it parses.
    pub fn write_entry(&self, id: &str, text: &str) -> Result<()> {
        if !valid_id(id) {
            return Err(Error::Lore(format!(
                "{id:?} is not an entry ID: segments of a-z, 0-9 and -, separated by /"
            )));
        }
        parse_entry(text)?;
        let path = self.entry_path(id);
        let io = |source| Error::Io {
            path: path.clone(),
            source,
        };
        let parent = path.parent().expect("an entry path has a parent");
        std::fs::create_dir_all(parent).map_err(io)?;
        let tmp = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("entry"),
            std::process::id()
        ));
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, &path).map_err(io)?;
        self.commit(&[path], &format!("{id}: text"), None);
        Ok(())
    }

    /// Strikes a companion's line: it is hidden from every reader.
    pub fn strike(&self, id: &str, line: &str, at: i64) -> Result<Line> {
        let entry = self
            .entry(id)?
            .ok_or_else(|| Error::Lore(format!("there is no entry {id:?}")))?;
        let target = find_line(&entry, line)?;
        let mut strike = Line::new(Kind::Strike, "user", "", at);
        strike.line = Some(target.id);
        self.append(id, std::slice::from_ref(&strike), None)?;
        Ok(target.clone())
    }

    /// Whether the directory is a git repository and git runs.
    pub fn has_git(&self) -> bool {
        self.dir.join(".git").exists() && git_available()
    }

    /// Makes the directory a git repository when git is installed and it
    /// isn't one. Returns whether it is one now.
    pub fn git_init(&self) -> bool {
        if self.dir.join(".git").exists() {
            return true;
        }
        git_available()
            && git(&self.dir)
                .args(["init", "-q"])
                .status()
                .is_ok_and(|s| s.success())
    }

    /// Commits `paths` alone: never what else the user has staged or changed.
    /// Without git, or when the commit fails, nothing happens: the files are
    /// what counts.
    fn commit(&self, paths: &[PathBuf], message: &str, author: Option<(&str, Uuid)>) {
        if !self.has_git() {
            return;
        }
        let rel: Vec<PathBuf> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(&self.dir).ok().map(Path::to_owned))
            .collect();
        let added = git(&self.dir)
            .arg("add")
            .arg("--")
            .args(&rel)
            .status()
            .is_ok_and(|s| s.success());
        if !added {
            return;
        }
        let mut commit = git(&self.dir);
        if let Some((name, id)) = author {
            let email = format!("{id}@clanka.invalid");
            commit
                .env("GIT_AUTHOR_NAME", name)
                .env("GIT_AUTHOR_EMAIL", &email)
                .env("GIT_COMMITTER_NAME", name)
                .env("GIT_COMMITTER_EMAIL", &email);
        }
        let _ = commit
            .args(["commit", "--no-verify", "-q", "-m", message, "--only", "--"])
            .args(&rel)
            .status();
    }
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// git in `dir`, with no hooks, no signing and no prompts.
fn git(dir: &Path) -> Command {
    let mut git = Command::new("git");
    git.current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    git
}

/// The line a short reference (or a whole ID) names.
pub fn find_line<'a>(entry: &'a Entry, reference: &str) -> Result<&'a Line> {
    let reference = reference.trim().trim_start_matches('#').to_lowercase();
    let found: Vec<&Line> = entry
        .lines
        .iter()
        .filter(|l| {
            matches!(l.kind, Kind::Deed | Kind::Note | Kind::Digest)
                && (l.id.to_string() == reference
                    || reference.len() >= 4 && l.id.simple().to_string().ends_with(&reference))
        })
        .collect();
    match found[..] {
        [line] => Ok(line),
        [] => Err(Error::Lore(format!(
            "{} has no line {reference:?}",
            entry.id
        ))),
        _ => Err(Error::Lore(format!(
            "{reference:?} names more than one line of {}; give more of its ID",
            entry.id
        ))),
    }
}

/// What a reader of an entry sees of its ledger.
#[derive(Debug, Default)]
pub struct View<'a> {
    /// The newest digest standing.
    pub digest: Option<&'a Line>,
    /// Deeds that make a sentence of the user's text untrue, with the
    /// sentence, until the user rewrites it.
    pub anchored: Vec<(&'a Line, &'a str)>,
    /// Lines after the digest, strongest first, newest first within each.
    pub lines: Vec<&'a Line>,
}

impl Entry {
    /// What stands of the ledger: the user's strikes hide a line, a writer's
    /// retraction or replacement hides their own, the newest digest folds
    /// everything before it, and an anchored deed stands over the sentence
    /// it changed until the user rewrites that sentence.
    pub fn view(&self) -> View<'_> {
        let struck: HashSet<Uuid> = self
            .lines
            .iter()
            .filter(|l| l.kind == Kind::Strike && l.by == "user")
            .filter_map(|l| l.line)
            .collect();
        let by_of = |id: Uuid| self.lines.iter().find(|l| l.id == id).map(|l| &l.by);
        let gone: HashSet<Uuid> = self
            .lines
            .iter()
            .filter_map(|l| {
                let target = match l.kind {
                    Kind::Retract => l.line?,
                    _ => l.replaces?,
                };
                (by_of(target) == Some(&l.by)).then_some(target)
            })
            .collect();
        let standing: Vec<&Line> = self
            .lines
            .iter()
            .filter(|l| matches!(l.kind, Kind::Deed | Kind::Note | Kind::Digest))
            .filter(|l| !struck.contains(&l.id) && !gone.contains(&l.id))
            .collect();
        let digest_at = standing.iter().rposition(|l| l.kind == Kind::Digest);
        let digest = digest_at.map(|i| standing[i]);
        let mut anchored = Vec::new();
        for anchor in standing
            .iter()
            .filter(|l| l.kind == Kind::Digest)
            .flat_map(|l| &l.anchors)
        {
            if let Some(deed) = standing
                .iter()
                .find(|l| l.id == anchor.line && l.kind == Kind::Deed)
                && said_verbatim(&self.text, &anchor.quote)
                && !anchored
                    .iter()
                    .any(|(d, _): &(&Line, &str)| d.id == deed.id)
            {
                anchored.push((*deed, anchor.quote.as_str()));
            }
        }
        let after = digest_at.map_or(0, |i| i + 1);
        let mut lines: Vec<(usize, &Line)> = standing[after..]
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, l)| l.kind != Kind::Digest)
            .collect();
        lines.sort_by_key(|(i, l)| {
            (
                l.provenance.unwrap_or(Provenance::WorkedOut),
                std::cmp::Reverse(*i),
            )
        });
        View {
            digest,
            anchored,
            lines: lines.into_iter().map(|(_, l)| l).collect(),
        }
    }

    /// Lines the keeper would fold: those after the last digest.
    pub fn unfolded(&self) -> usize {
        self.view().lines.len()
    }

    /// The hash read receipts keep of the user's text.
    pub fn text_hash(&self) -> String {
        sha256_hex(&self.text)
    }

    /// The newest line someone other than `me` wrote.
    fn newest_from_others(&self, me: &str) -> Option<String> {
        self.lines
            .iter()
            .rev()
            .find(|l| l.by != me)
            .map(|l| l.id.to_string())
    }

    /// Every name the entry goes by.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.front.title.as_str())
            .chain(self.front.aliases.iter().map(String::as_str))
            .filter(|n| !n.trim().is_empty())
    }
}

/// One of the entry's lines as a reader sees it. `me` is the reader's ID.
fn render_line(line: &Line, me: &str) -> String {
    let who = if line.by == me { "You" } else { &line.name };
    let label = line.provenance.unwrap_or(Provenance::WorkedOut).label();
    format!("- {who} ({label}) [{}]: {}", line.short(), line.text)
}

/// An open entry, within `room` estimated tokens: `None` when the user's text
/// alone doesn't fit. Lines that don't fit are left out, weakest first.
fn render_entry(
    entry: &Entry,
    me: &str,
    notes: &[(String, Provenance)],
    changed: bool,
    room: usize,
) -> Option<String> {
    let mut head = format!("### {} ({})", entry.front.title, entry.id);
    if entry.front.sealed {
        head.push_str(", sealed: nothing you learn here is shared");
    }
    if changed {
        head.push_str(", changed since you last read it");
    }
    let text = if entry.stub {
        "Nobody has written this down yet.".to_owned()
    } else {
        entry.text.clone()
    };
    let mut out = format!("{head}\n{text}");
    if estimate_tokens(&out) > room {
        return None;
    }
    let view = entry.view();
    let mut extra = Vec::new();
    for (deed, quote) in &view.anchored {
        extra.push(format!(
            "- No longer so: \"{quote}\". {}",
            render_line(deed, me).trim_start_matches("- ")
        ));
    }
    if let Some(digest) = view.digest {
        let label = digest.provenance.unwrap_or(Provenance::WorkedOut).label();
        extra.push(format!("- Since then ({label}): {}", digest.text));
    }
    extra.extend(
        view.lines
            .iter()
            .take(SHOWN_LINES)
            .map(|l| render_line(l, me)),
    );
    let mut used = estimate_tokens(&out);
    for line in extra {
        let cost = estimate_tokens(&line) + 1;
        if used + cost > room {
            break;
        }
        used += cost;
        out.push('\n');
        out.push_str(&line);
    }
    if !notes.is_empty() {
        let mut first = true;
        for (note, provenance) in notes.iter().take(SHOWN_NOTES) {
            let line = format!("- {note} ({})", provenance.label());
            let heading = if first { "Your private notes:\n" } else { "" };
            let cost = estimate_tokens(&line) + estimate_tokens(heading) + 1;
            if used + cost > room {
                break;
            }
            used += cost;
            out.push('\n');
            out.push_str(heading);
            out.push_str(&line);
            first = false;
        }
    }
    Some(out)
}

/// Said once, above the open entries.
const LORE_HEADING: &str = "## Lore\nHow the world is: the text of each entry is canon. Lines \
    under it are what companions added: seen and told are as good as canon, claimed is their \
    word only, worked out and dreamt may be wrong.";

/// Common words: as a sentence's first word none is a name, and none says
/// what a note claims.
const COMMON: &[&str] = &[
    "a", "an", "the", "i", "i'm", "i've", "i'd", "i'll", "you", "he", "she", "it", "we", "they",
    "my", "your", "his", "her", "its", "our", "their", "this", "that", "these", "those", "there",
    "here", "in", "on", "at", "of", "to", "for", "from", "with", "by", "when", "where", "while",
    "if", "but", "and", "or", "so", "then", "after", "before", "some", "no", "not", "all", "what",
    "who", "how", "why", "once", "every", "each", "now", "nobody", "someone", "everyone", "only",
    "even", "still", "yes", "maybe",
];

/// The proper names and numbers in a text: what a note's provenance checks.
/// Capitalised words that aren't common sentence openers, and words with a
/// digit in them. `own` (the writer's name) is left out.
pub fn checkable(text: &str, own: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split_whitespace() {
        let word = raw.trim_matches(|c: char| !c.is_alphanumeric());
        let word = word
            .strip_suffix("'s")
            .or_else(|| word.strip_suffix("\u{2019}s"))
            .unwrap_or(word);
        if word.is_empty() || word.eq_ignore_ascii_case(own) {
            continue;
        }
        let number = word.chars().any(|c| c.is_ascii_digit());
        let mut chars = word.chars();
        let name = chars.next().is_some_and(char::is_uppercase)
            && word.chars().any(char::is_lowercase)
            && !COMMON.contains(&word.to_lowercase().as_str());
        if (number || name) && !out.iter().any(|w| w == word) {
            out.push(word.to_owned());
        }
    }
    out
}

/// Whether a note is told: every name and number in it is in `sources`, and
/// so are most of its other longer words, so a paraphrase of what was said
/// counts and a guess dressed in known names doesn't. Common words and the
/// writer's own name count neither way.
pub fn is_told(note: &str, own: &str, sources: &[&str]) -> bool {
    let source = sources.join("\n").to_lowercase();
    if !checkable(note, own)
        .iter()
        .all(|w| source.contains(&w.to_lowercase()))
    {
        return false;
    }
    let content: Vec<String> = note
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_lowercase)
        .filter(|w| !COMMON.contains(&w.as_str()) && !w.eq_ignore_ascii_case(own))
        .collect();
    !content.is_empty()
        && content.iter().filter(|w| source.contains(*w)).count() * 2 > content.len()
}

/// One line as the ledger takes it: newlines and Markdown headings or
/// frontmatter fences at its start can't break the file or the prompt.
pub fn clean_line(text: &str, limit: usize) -> Result<String> {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let one = one
        .trim_start_matches(|c: char| c == '#' || c == '+' || c.is_whitespace())
        .trim()
        .to_owned();
    if one.is_empty() {
        return Err(Error::Rule("lore_empty", "the line is empty".into()));
    }
    let chars = one.chars().count();
    if chars > limit {
        return Err(Error::Rule(
            "lore_too_long",
            format!("the line is {chars} characters; at most {limit}"),
        ));
    }
    Ok(one)
}

/// A title as an entry ID segment: `The Old Mill` is `the-old-mill`.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Where a name appears in a message as whole words, in any case: the byte
/// span of each time, in the lowercased message.
fn named_at(message: &str, name: &str) -> Vec<(usize, usize)> {
    let message = message.to_lowercase();
    let name = name.trim().to_lowercase();
    let mut out = Vec::new();
    if name.chars().count() < 3 {
        return out;
    }
    let mut from = 0;
    while let Some(i) = message[from..].find(&name) {
        let start = from + i;
        let end = start + name.len();
        let before = message[..start].chars().next_back();
        let after = message[end..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            out.push((start, end));
        }
        from = start + name.chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// Where a lore line's evidence comes from: the exchange a reflection is on,
/// or a whole dream at waking.
pub(crate) struct Heard<'a> {
    pub session: i64,
    /// What the user said that the companion heard.
    pub user: &'a str,
    /// What the companion replied.
    pub reply: &'a str,
    pub dream: bool,
}

/// A private note of the companion's, as `clanka lore notes` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: i64,
    pub entry: String,
    pub text: String,
    pub provenance: Provenance,
    /// The ledger line it was released as.
    pub released: Option<String>,
    pub created_at: i64,
}

impl Companion {
    /// Where this handle's lore lives; `None` leaves lore out of every turn.
    pub fn set_lore(&mut self, lore: Option<Lore>) {
        self.lore = lore;
    }

    pub fn lore(&self) -> Option<&Lore> {
        self.lore.as_ref()
    }

    /// The lore's problems this companion would read: about an entry it may
    /// read, or a name shared by two or more of them.
    pub fn lore_problems(&self) -> Result<Vec<String>> {
        let Some(lore) = &self.lore else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for problem in lore.problems()? {
            let mut readable = 0;
            for id in &problem.entries {
                readable += usize::from(self.may_read(id)?);
            }
            if readable > 0 && readable >= problem.entries.len().min(2) {
                out.push(problem.text);
            }
        }
        Ok(out)
    }

    /// Whether the companion may read entry `id`: its soul grants it, or an
    /// entry it read opens it, or it made the stub.
    pub fn may_read(&self, id: &str) -> Result<bool> {
        if self.soul.traits.lore.iter().any(|g| grants(g, id)) {
            return Ok(true);
        }
        Ok(self
            .conn
            .query_row("SELECT 1 FROM lore_earned WHERE entry = ?1", [id], |_| {
                Ok(())
            })
            .optional()?
            .is_some())
    }

    /// Every entry the companion may read, sorted by ID.
    pub fn readable_lore(&self) -> Result<Vec<Entry>> {
        let Some(lore) = &self.lore else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for id in lore.ids()? {
            // One broken entry mustn't take every turn down with it.
            if self.may_read(&id)?
                && let Ok(Some(entry)) = lore.entry(&id)
            {
                out.push(entry);
            }
        }
        Ok(out)
    }

    /// The entry an ID, a title or an alias names, among those the companion
    /// may read.
    fn find_lore(&self, query: &str) -> Result<Entry> {
        let query = query.trim();
        let readable = self.readable_lore()?;
        let named = |n: &str| n.trim().eq_ignore_ascii_case(query);
        // An entry's own title wins over another entry's alias.
        let found = readable
            .iter()
            .find(|e| e.id == query)
            .or_else(|| readable.iter().find(|e| named(&e.front.title)))
            .or_else(|| readable.iter().find(|e| e.names().any(named)));
        match found {
            Some(entry) => Ok(entry.clone()),
            None if readable.is_empty() => Err(Error::Lore(format!(
                "there is no entry {query:?}; you know of no lore"
            ))),
            None => {
                let ids: Vec<&str> = readable.iter().map(|e| e.id.as_str()).collect();
                Err(Error::Lore(format!(
                    "there is no entry {query:?} you know of; you know of {}",
                    ids.join(", ")
                )))
            }
        }
    }

    fn open_conversation(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM sessions WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Opens an entry in a session, read at `read_at`: it stays open until
    /// the allotment has no room left for it beside entries read more
    /// recently. What the entry `opens` the companion may read from now on.
    fn open_entry(&self, session: i64, entry: &Entry, read_at: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO lore_open (session_id, entry, read_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (session_id, entry) DO UPDATE SET read_at = excluded.read_at",
            params![session, entry.id, read_at],
        )?;
        for id in &entry.front.opens {
            tx.execute(
                "INSERT OR IGNORE INTO lore_earned (entry, via, earned_at) VALUES (?1, ?2, ?3)",
                params![id, entry.id, read_at],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Entries open in a session that are still there and readable, read
    /// most recently first.
    pub(crate) fn open_lore(&self, session: i64) -> Result<Vec<Entry>> {
        let Some(lore) = &self.lore else {
            return Ok(Vec::new());
        };
        let ids: Vec<String> = self
            .conn
            .prepare(
                "SELECT entry FROM lore_open WHERE session_id = ?1 ORDER BY read_at DESC, rowid DESC",
            )?
            .query_map([session], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::new();
        for id in ids {
            if self.may_read(&id)?
                && let Ok(Some(entry)) = lore.entry(&id)
            {
                out.push(entry);
            }
        }
        Ok(out)
    }

    /// Whether the entry changed since the companion last read it: the
    /// user's text, or a line someone else added.
    fn changed_since_read(&self, entry: &Entry) -> Result<bool> {
        let read: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT text_hash, last_line FROM lore_reads WHERE entry = ?1",
                [&entry.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(read.is_some_and(|(hash, last)| {
            hash != entry.text_hash() || last != entry.newest_from_others(&self.id().to_string())
        }))
    }

    fn mark_read(&self, entry: &Entry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO lore_reads (entry, text_hash, last_line, read_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (entry) DO UPDATE SET text_hash = excluded.text_hash,
               last_line = excluded.last_line, read_at = excluded.read_at",
            params![
                entry.id,
                entry.text_hash(),
                entry.newest_from_others(&self.id().to_string()),
                self.deps.clock.now_ms()
            ],
        )?;
        Ok(())
    }

    /// The companion's private notes on an entry, newest first.
    fn private_notes(&self, entry: &str) -> Result<Vec<(String, Provenance)>> {
        Ok(self
            .conn
            .prepare(
                "SELECT text, provenance FROM lore_notes
                 WHERE entry = ?1 AND released_line IS NULL ORDER BY id DESC",
            )?
            .query_map([entry], |r| {
                let provenance: String = r.get(1)?;
                Ok((
                    r.get(0)?,
                    Provenance::parse(&provenance).unwrap_or(Provenance::WorkedOut),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The entry as the companion reads it, within `room`, and its read
    /// receipt updated. `None` when the user's text alone doesn't fit.
    fn read_entry(&self, entry: &Entry, room: usize) -> Result<Option<String>> {
        let changed = self.changed_since_read(entry)?;
        let notes = self.private_notes(&entry.id)?;
        let rendered = render_entry(entry, &self.id().to_string(), &notes, changed, room);
        if rendered.is_some() {
            self.mark_read(entry)?;
        }
        Ok(rendered)
    }

    /// What the `read_lore` tool and `/lore <id>` return: the entry an ID,
    /// title or alias names, which stays open for the rest of the conversation.
    pub fn read_lore(&mut self, query: &str) -> Result<String> {
        let Some(tokens) = self.lore.as_ref().map(|l| l.tokens) else {
            return Err(Error::Lore("no lore is set up here".into()));
        };
        let entry = self.find_lore(query)?;
        let text = self.read_entry(&entry, tokens)?.ok_or_else(|| {
            Error::Lore(format!(
                "{} is longer than the {tokens} tokens lore has; it can't be read",
                entry.id
            ))
        })?;
        if let Some(session) = self.open_conversation()? {
            self.open_entry(session, &entry, self.deps.clock.now_ms())?;
            self.conn.execute(
                "UPDATE lore_open SET shown = ?3 WHERE session_id = ?1 AND entry = ?2",
                params![session, entry.id, sha256_hex(&text)],
            )?;
        }
        Ok(text)
    }

    /// The lore part of a turn's context: entries the message names open
    /// first, then every open entry within the allotment, read most recently
    /// first, and an index of the others. On a later turn of a host that
    /// replays its history (`first` false), only what it hasn't shown yet.
    pub fn lore_context(&self, session: i64, message: &str, first: bool) -> Result<Option<String>> {
        let Some(room) = self.lore.as_ref().map(|l| l.tokens) else {
            return Ok(None);
        };
        let readable = self.readable_lore()?;
        if readable.is_empty() {
            return Ok(None);
        }
        // Where the message names each entry. A name inside another entry's
        // longer name ("guard" in "the Obsidian Guard") doesn't count.
        let found: Vec<(&Entry, Vec<(usize, usize)>)> = readable
            .iter()
            .map(|e| (e, e.names().flat_map(|n| named_at(message, n)).collect()))
            .collect();
        let inside = |id: &str, (s, e): (usize, usize)| {
            found.iter().any(|(other, theirs)| {
                other.id != id
                    && theirs
                        .iter()
                        .any(|&(a, b)| a <= s && e <= b && b - a > e - s)
            })
        };
        let mut named: Vec<(usize, &Entry)> = found
            .iter()
            .filter_map(|(entry, spans)| {
                let at = spans
                    .iter()
                    .filter(|&&span| !inside(&entry.id, span))
                    .min()?;
                Some((at.0, *entry))
            })
            .collect();
        named.sort_by_key(|&(at, _)| at);
        // The first named reads as the most recent, so it is the last to
        // close when the allotment runs out.
        let now = self.deps.clock.now_ms();
        for (i, (_, entry)) in named.iter().enumerate() {
            self.open_entry(session, entry, now - i as i64)?;
        }
        let open = self.open_lore(session)?;
        let full = room.saturating_sub(estimate_tokens(LORE_HEADING));
        let mut left = full;
        let mut kept = Vec::new();
        let mut shown = Vec::new();
        let mut closed = Vec::new();
        for entry in &open {
            match self.read_entry(entry, left)? {
                Some(text) => {
                    kept.push(entry.id.as_str());
                    left = left.saturating_sub(estimate_tokens(&text) + 1);
                    let hash = sha256_hex(&text);
                    let before: Option<String> = self.conn.query_row(
                        "SELECT shown FROM lore_open WHERE session_id = ?1 AND entry = ?2",
                        params![session, entry.id],
                        |r| r.get(0),
                    )?;
                    if first || before.as_deref() != Some(hash.as_str()) {
                        shown.push(text);
                    }
                    self.conn.execute(
                        "UPDATE lore_open SET shown = ?3 WHERE session_id = ?1 AND entry = ?2",
                        params![session, entry.id, hash],
                    )?;
                }
                None => {
                    // No room beside what was read more recently: it closes
                    // until it is named again. Said only when it doesn't fit
                    // even alone.
                    self.conn.execute(
                        "DELETE FROM lore_open WHERE session_id = ?1 AND entry = ?2",
                        params![session, entry.id],
                    )?;
                    if left == full {
                        closed.push(entry.id.as_str());
                    }
                }
            }
        }
        let mut index = Vec::new();
        if first {
            for entry in readable
                .iter()
                .filter(|e| !kept.contains(&e.id.as_str()) && !closed.contains(&e.id.as_str()))
                .take(INDEX_LIMIT)
            {
                let mut line = format!("- {}: {}", entry.id, entry.front.title);
                if self.changed_since_read(entry)? {
                    line.push_str(" (changed since you read it)");
                }
                let cost = estimate_tokens(&line) + 1;
                if cost > left {
                    break;
                }
                left -= cost;
                index.push(line);
            }
        }
        if shown.is_empty() && index.is_empty() && closed.is_empty() {
            return Ok(None);
        }
        let mut out = vec![LORE_HEADING.to_owned()];
        out.extend(shown);
        if !closed.is_empty() {
            out.push(format!("Too long to keep in mind: {}.", closed.join(", ")));
        }
        if !index.is_empty() {
            out.push(format!(
                "Other entries you know of; one opens when it is named:\n{}",
                index.join("\n")
            ));
        }
        Ok(Some(out.join("\n\n")))
    }

    /// The lore tools and the part of the prompt they need, for a pass over
    /// `session`, or `None` when no entry is open in it. A dream's waking
    /// gets only `lore_note`: nothing done in a dream happened.
    pub(crate) fn lore_tools(
        &self,
        session: i64,
        dream: bool,
    ) -> Result<Option<(Vec<ToolSpec>, String)>> {
        let open = self.open_lore(session)?;
        if open.is_empty() {
            return Ok(None);
        }
        let me = self.id().to_string();
        let mut listed = Vec::new();
        for entry in &open {
            let mut line = format!("- {}: {}", entry.id, entry.front.title);
            if entry.front.sealed {
                line.push_str(" (sealed: notes stay private, no deeds)");
            }
            let view = entry.view();
            for own in view.lines.iter().filter(|l| l.by == me) {
                line.push_str(&format!(
                    "\n  your line [{}] {:?}: {}",
                    own.short(),
                    own.kind,
                    own.text
                ));
            }
            listed.push(line);
        }
        let stub_made = self.stub_made(session)?;
        let tools = lore_tool_specs()
            .into_iter()
            .filter(|t| !dream || t.name == "lore_note")
            .filter(|t| !(stub_made && t.name == "lore_stub"))
            .collect();
        let prompt = format!(
            "Lore open in this conversation, by ID (the world's reference; what you add to it \
             is for every companion who reads it, at most {LORE_CALLS} lore calls):\n{}",
            listed.join("\n")
        );
        Ok(Some((tools, prompt)))
    }

    fn stub_made(&self, session: i64) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM lore_outbox WHERE session_id = ?1 AND kind = 'stub'",
                [session],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// An entry open in the session the companion may write about.
    fn open_for_writing(&self, session: i64, id: &str) -> Result<Entry> {
        self.open_lore(session)?
            .into_iter()
            .find(|e| e.id == id.trim())
            .ok_or_else(|| {
                Error::Rule(
                    "lore_not_open",
                    format!("{id:?} is not an entry open in this conversation"),
                )
            })
    }

    /// Lore files are plaintext anyone with the directory reads: a line that
    /// holds the user's name, or anything they told the companion about
    /// themselves, stays private.
    fn check_public(&self, text: &str) -> Result<()> {
        let mut private: Vec<String> = self.user_name()?.into_iter().collect();
        for memory in self.told()? {
            for value in memory.facts.values() {
                if let Some(value) = value.as_str() {
                    private.push(value.to_owned());
                }
            }
        }
        for value in private {
            if value.trim().chars().count() >= 3 && said_verbatim(text, &value) {
                return Err(Error::Rule(
                    "lore_private",
                    format!(
                        "it holds {value:?}, which they told you about themselves; lore is read \
                         by others, so keep it as a private note (release false) or leave it out"
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Runs one lore tool call.
    pub(crate) fn run_lore_tool(&mut self, call: &ToolCall, heard: &Heard) -> Result<String> {
        match call.name.as_str() {
            "lore_note" => {
                let a: NoteArgs = args(call)?;
                self.lore_note(&a.entry, &a.text, a.release, heard)
            }
            "lore_deed" if !heard.dream => {
                let a: DeedArgs = args(call)?;
                self.lore_deed(&a.entry, &a.text, &a.quote, heard)
            }
            "lore_retract" if !heard.dream => {
                let a: RetractArgs = args(call)?;
                self.lore_retract(&a.line, a.text.as_deref(), heard)
            }
            "lore_stub" if !heard.dream => {
                let a: StubArgs = args(call)?;
                self.lore_stub(&a.title, &a.near, heard)
            }
            other => Err(Error::Invalid(format!("there is no tool called {other:?}"))),
        }
    }

    fn lore_note(
        &mut self,
        entry: &str,
        text: &str,
        release: bool,
        heard: &Heard,
    ) -> Result<String> {
        let entry = self.open_for_writing(heard.session, entry)?;
        let text = clean_line(text, LINE_CHARS)?;
        if release && entry.front.sealed {
            return Err(Error::Rule(
                "lore_sealed",
                format!(
                    "{} is sealed: what you learn there stays yours; call again with release false",
                    entry.id
                ),
            ));
        }
        if release {
            self.check_public(&text)?;
        }
        let name = &self.soul.traits.name;
        let told = is_told(&text, name, &[heard.user, &entry.text]);
        let provenance = match (heard.dream, self.soul.traits.dreams, told) {
            (false, _, true) | (true, Dreams::Shared, true) => Provenance::Told,
            (false, _, false) => Provenance::WorkedOut,
            (true, _, _) => Provenance::Dreamt,
        };
        let now = self.deps.clock.now_ms();
        let line = release.then(|| {
            let mut line = Line::new(Kind::Note, &self.id().to_string(), name, now);
            line.provenance = Some(provenance);
            line.text = text.clone();
            line
        });
        let tx = self.conn.unchecked_transaction()?;
        crate::companion::ensure_writable(&tx, &self.soul.hash)?;
        tx.execute(
            "INSERT INTO lore_notes (entry, text, provenance, session_id, released_line, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.id,
                text,
                provenance.as_str(),
                heard.session,
                line.as_ref().map(|l| l.id.to_string()),
                now
            ],
        )?;
        if let Some(line) = &line {
            outbox(&tx, &entry.id, line, heard.session, now)?;
        }
        tx.commit()?;
        let Some(line) = line else {
            return Ok(format!(
                "private note kept on {}, {}",
                entry.id,
                provenance.label()
            ));
        };
        let _ = self.flush_lore();
        Ok(format!(
            "note [{}] released on {}, {}",
            line.short(),
            entry.id,
            provenance.label()
        ))
    }

    fn lore_deed(&mut self, entry: &str, text: &str, quote: &str, heard: &Heard) -> Result<String> {
        let entry = self.open_for_writing(heard.session, entry)?;
        if entry.front.sealed {
            return Err(Error::Rule(
                "lore_sealed",
                format!(
                    "{} is sealed: nothing about it is shared; keep a private note instead",
                    entry.id
                ),
            ));
        }
        let text = clean_line(text, LINE_CHARS)?;
        self.check_public(&text)?;
        let provenance = deed_provenance(quote, heard)?;
        let now = self.deps.clock.now_ms();
        let mut line = Line::new(
            Kind::Deed,
            &self.id().to_string(),
            &self.soul.traits.name,
            now,
        );
        line.provenance = Some(provenance);
        line.text = text;
        self.publish(&entry.id, &line, heard.session)?;
        Ok(format!(
            "deed [{}] recorded on {}, {}",
            line.short(),
            entry.id,
            provenance.label()
        ))
    }

    fn lore_retract(
        &mut self,
        reference: &str,
        text: Option<&str>,
        heard: &Heard,
    ) -> Result<String> {
        let me = self.id().to_string();
        let mut found = None;
        for entry in self.open_lore(heard.session)? {
            if let Ok(line) = find_line(&entry, reference)
                && line.by == me
                && matches!(line.kind, Kind::Deed | Kind::Note)
            {
                found = Some((entry.clone(), line.clone()));
            }
        }
        let (entry, old) = found.ok_or_else(|| {
            Error::Rule(
                "lore_not_yours",
                format!("{reference:?} is no line of yours under an open entry"),
            )
        })?;
        let now = self.deps.clock.now_ms();
        let name = &self.soul.traits.name;
        let line = match text.map(str::trim).filter(|t| !t.is_empty()) {
            Some(text) => {
                let text = clean_line(text, LINE_CHARS)?;
                self.check_public(&text)?;
                let mut line = Line::new(old.kind, &me, name, now);
                // A replaced deed keeps the evidence it had; a note is checked again.
                line.provenance = match old.kind {
                    Kind::Note if is_told(&text, name, &[heard.user, &entry.text]) => {
                        Some(Provenance::Told)
                    }
                    Kind::Note => Some(Provenance::WorkedOut),
                    _ => old.provenance,
                };
                line.text = text;
                line.replaces = Some(old.id);
                line
            }
            None => {
                let mut line = Line::new(Kind::Retract, &me, name, now);
                line.line = Some(old.id);
                line
            }
        };
        self.publish(&entry.id, &line, heard.session)?;
        Ok(match line.kind {
            Kind::Retract => format!("line [{}] withdrawn", old.short()),
            _ => format!("line [{}] replaced by [{}]", old.short(), line.short()),
        })
    }

    fn lore_stub(&mut self, title: &str, near: &str, heard: &Heard) -> Result<String> {
        let near = self.open_for_writing(heard.session, near)?;
        if self.stub_made(heard.session)? {
            return Err(Error::Rule(
                "lore_stub_made",
                "one new entry a conversation; this one has its own already".into(),
            ));
        }
        let title = clean_line(title, TITLE_CHARS)?;
        if !said_verbatim(heard.user, &title) {
            return Err(Error::Rule(
                "lore_stub_unsaid",
                format!("{title:?} is not what they called it; use their exact words"),
            ));
        }
        self.check_public(&title)?;
        let lore = self.lore.clone().expect("an entry is open, so lore is set");
        let segment = slug(&title);
        if segment.is_empty() {
            return Err(Error::Rule(
                "lore_stub_unnamed",
                format!("{title:?} has no letters or digits to name an entry by"),
            ));
        }
        let dir = near.id.rsplit_once('/').map(|(dir, _)| dir);
        let id = match dir {
            Some(dir) => format!("{dir}/{segment}"),
            None => segment.clone(),
        };
        let siblings: Vec<String> = lore
            .ids()?
            .into_iter()
            .filter(|other| other.rsplit_once('/').map(|(d, _)| d) == dir)
            .collect();
        if let Some(close) = siblings.iter().find(|other| {
            let last = other.rsplit('/').next().unwrap_or(other);
            edit_distance(last, &segment) <= 2 || last.contains(&segment) || segment.contains(last)
        }) {
            return Err(Error::Rule(
                "lore_stub_exists",
                format!("there is an entry {close:?} already; that may be it"),
            ));
        }
        let now = self.deps.clock.now_ms();
        let mut line = Line::new(
            Kind::Stub,
            &self.id().to_string(),
            &self.soul.traits.name,
            now,
        );
        line.text = title;
        self.conn.execute(
            "INSERT OR IGNORE INTO lore_earned (entry, via, earned_at) VALUES (?1, 'stub', ?2)",
            params![id, now],
        )?;
        self.publish(&id, &line, heard.session)?;
        Ok(format!("new entry {id}: nobody has written it yet"))
    }

    /// Queues a line for the entry's ledger and appends what is queued.
    fn publish(&mut self, entry: &str, line: &Line, session: i64) -> Result<()> {
        let now = self.deps.clock.now_ms();
        let tx = self.conn.unchecked_transaction()?;
        crate::companion::ensure_writable(&tx, &self.soul.hash)?;
        outbox(&tx, entry, line, session, now)?;
        tx.commit()?;
        // The line is kept: one that can't be appended now is the next flush's.
        let _ = self.flush_lore();
        Ok(())
    }

    /// Appends the lines the companion queued to their ledgers. A frozen
    /// companion publishes nothing; what is queued waits. Returns how many
    /// lines were appended.
    pub fn flush_lore(&mut self) -> Result<usize> {
        let Some(lore) = self.lore.clone() else {
            return Ok(0);
        };
        if self.is_frozen()? {
            return Ok(0);
        }
        let queued: Vec<(i64, String, String)> = self
            .conn
            .prepare(
                "SELECT id, entry, line FROM lore_outbox WHERE flushed_at IS NULL ORDER BY id",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut done = 0;
        let author = (self.soul.traits.name.clone(), self.id());
        let mut entries: Vec<&str> = queued.iter().map(|(_, e, _)| e.as_str()).collect();
        entries.sort_unstable();
        entries.dedup();
        for entry in entries {
            let rows: Vec<&(i64, String, String)> =
                queued.iter().filter(|(_, e, _)| e == entry).collect();
            let lines: Vec<Line> = rows
                .iter()
                .filter_map(|(_, _, line)| serde_json::from_str(line).ok())
                .collect();
            lore.append(entry, &lines, Some((&author.0, author.1)))?;
            let now = self.deps.clock.now_ms();
            for (id, _, _) in rows {
                self.conn.execute(
                    "UPDATE lore_outbox SET flushed_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
                done += 1;
            }
        }
        Ok(done)
    }

    /// The companion's private and released notes, newest first, on one
    /// entry or all of them.
    pub fn lore_notes(&self, entry: Option<&str>) -> Result<Vec<Note>> {
        Ok(self
            .conn
            .prepare(
                "SELECT id, entry, text, provenance, released_line, created_at FROM lore_notes
                 WHERE ?1 IS NULL OR entry = ?1 ORDER BY id DESC",
            )?
            .query_map([entry], |r| {
                let provenance: String = r.get(3)?;
                Ok(Note {
                    id: r.get(0)?,
                    entry: r.get(1)?,
                    text: r.get(2)?,
                    provenance: Provenance::parse(&provenance).unwrap_or(Provenance::WorkedOut),
                    released: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Deletes one private note: `clanka lore forget`, the only way a note
    /// goes. A released one stays in the ledger, where the user strikes it.
    pub fn forget_lore_note(&mut self, id: i64) -> Result<Note> {
        let note = self
            .lore_notes(None)?
            .into_iter()
            .find(|n| n.id == id)
            .ok_or(Error::NotFound(id))?;
        let tx = self.conn.unchecked_transaction()?;
        crate::companion::ensure_writable(&tx, &self.soul.hash)?;
        tx.execute("DELETE FROM lore_notes WHERE id = ?1", [id])?;
        crate::companion::log(
            &tx,
            "cli",
            "lore_forget",
            "lore_note",
            Some(id),
            &json!({ "entry": note.entry }),
            self.deps.clock.now_ms(),
        )?;
        tx.commit()?;
        Ok(note)
    }

    /// The keeper: after a night is consolidated, each entry the companion
    /// added to that night and that has more than [`KEEPER_AFTER`] lines since
    /// its last digest gets a new digest, written without the persona.
    /// Returns how many digests were written. A refused digest changes nothing.
    pub fn keep_lore(&mut self, session: i64) -> Result<usize> {
        let ids: Vec<String> = self
            .conn
            .prepare("SELECT DISTINCT entry FROM lore_outbox WHERE session_id = ?1 ORDER BY entry")?
            .query_map([session], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut written = 0;
        for id in ids {
            written += usize::from(self.digest_entry(&id, session, false)?);
        }
        Ok(written)
    }

    /// Writes a digest of an entry now, as `clanka lore redigest` asks, however
    /// few its lines. Returns whether one was written.
    pub fn redigest(&mut self, id: &str) -> Result<bool> {
        let session = self.open_conversation()?;
        self.digest_entry(id, session.unwrap_or(0), true)
    }

    fn digest_entry(&mut self, id: &str, session: i64, always: bool) -> Result<bool> {
        let Some(lore) = self.lore.clone() else {
            return Ok(false);
        };
        let Some(entry) = lore.entry(id)? else {
            return Ok(false);
        };
        let lines: Vec<Line> = entry.view().lines.into_iter().cloned().collect();
        let enough = if always {
            !lines.is_empty()
        } else {
            lines.len() > KEEPER_AFTER
        };
        if entry.front.sealed || !enough {
            return Ok(false);
        }
        let started = std::time::Instant::now();
        let listed: Vec<String> = lines
            .iter()
            .map(|l| {
                format!(
                    "[{}] {:?} by {} ({}): {}",
                    l.short(),
                    l.kind,
                    l.name,
                    l.provenance.unwrap_or(Provenance::WorkedOut).label(),
                    l.text
                )
            })
            .collect();
        let earlier = entry
            .view()
            .digest
            .map(|d| format!("\n\nThe digest so far:\n{}", d.text))
            .unwrap_or_default();
        let prompt = format!(
            "The entry {} ({}), as its author wrote it:\n{}{earlier}\n\nLines added since:\n{}",
            entry.front.title,
            entry.id,
            entry.text,
            listed.join("\n")
        );
        let mut written = false;
        let outcome = self.tool_loop(
            Pass::Keeper,
            KEEPER_INSTRUCTIONS.to_owned(),
            prompt,
            vec![digest_spec()],
            1,
            &[],
            |c, call| {
                let done = c.run_digest(call, &entry, &lines, session)?;
                written = true;
                Ok(done)
            },
        );
        self.diagnose_pass(
            Pass::Keeper,
            started,
            false,
            outcome.as_ref().ok(),
            outcome.as_ref().err(),
        );
        outcome?;
        Ok(written)
    }

    fn run_digest(
        &mut self,
        call: &ToolCall,
        entry: &Entry,
        lines: &[Line],
        session: i64,
    ) -> Result<String> {
        if call.name != "digest" {
            return Err(Error::Invalid(format!(
                "there is no tool called {:?}",
                call.name
            )));
        }
        let a: DigestArgs = args(call)?;
        let text = clean_line(&a.text, DIGEST_CHARS)?;
        let mut cited = Vec::new();
        for reference in &a.cites {
            let line = lines
                .iter()
                .find(|l| l.short() == reference.trim().trim_start_matches('#'))
                .ok_or_else(|| {
                    Error::Rule(
                        "lore_uncited",
                        format!("{reference:?} is none of the lines listed"),
                    )
                })?;
            cited.push(line);
        }
        if cited.is_empty() {
            return Err(Error::Rule(
                "lore_uncited",
                "a digest cites the lines it folds".into(),
            ));
        }
        let mut sources: Vec<&str> = vec![&entry.text];
        for line in &cited {
            sources.push(&line.text);
            sources.push(&line.name);
        }
        if let Some(digest) = entry.view().digest {
            sources.push(&digest.text);
        }
        let source = sources.join("\n").to_lowercase();
        let invented: Vec<String> = checkable(&text, "")
            .into_iter()
            .filter(|w| !source.contains(&w.to_lowercase()))
            .collect();
        if !invented.is_empty() {
            return Err(Error::Rule(
                "lore_invented",
                format!(
                    "{} appear in no cited line and not in the entry",
                    invented.join(", ")
                ),
            ));
        }
        self.check_public(&text)?;
        let mut anchors = Vec::new();
        for anchor in &a.anchors {
            let deed = cited
                .iter()
                .find(|l| l.short() == anchor.line.trim().trim_start_matches('#'))
                .filter(|l| l.kind == Kind::Deed)
                .ok_or_else(|| {
                    Error::Rule(
                        "lore_anchor",
                        format!("{:?} is no deed this digest cites", anchor.line),
                    )
                })?;
            if anchor.quote.split_whitespace().count() < DEED_WORDS
                || !said_verbatim(&entry.text, &anchor.quote)
            {
                return Err(Error::Rule(
                    "lore_anchor",
                    format!(
                        "{:?} is not a sentence of the entry, quoted exactly",
                        anchor.quote
                    ),
                ));
            }
            anchors.push(Anchor {
                line: deed.id,
                quote: anchor.quote.trim().to_owned(),
            });
        }
        let now = self.deps.clock.now_ms();
        let mut line = Line::new(
            Kind::Digest,
            &self.id().to_string(),
            &self.soul.traits.name,
            now,
        );
        line.provenance = cited.iter().filter_map(|l| l.provenance).max();
        line.text = text;
        line.cites = cited.iter().map(|l| l.id).collect();
        line.anchors = anchors;
        self.publish(&entry.id, &line, session)?;
        Ok(format!("digest [{}] written", line.short()))
    }
}

/// A deed's provenance from the words it quotes: the user's are narration,
/// the companion's own only a claim.
fn deed_provenance(quote: &str, heard: &Heard) -> Result<Provenance> {
    if quote.split_whitespace().count() < DEED_WORDS {
        return Err(Error::Rule(
            "lore_quote",
            format!("quote at least {DEED_WORDS} words of the exchange that show it happened"),
        ));
    }
    if said_verbatim(heard.user, quote) {
        Ok(Provenance::Narrated)
    } else if said_verbatim(heard.reply, quote) {
        Ok(Provenance::Claimed)
    } else {
        Err(Error::Rule(
            "lore_quote",
            format!("{quote:?} isn't in the exchange; copy the words exactly"),
        ))
    }
}

fn outbox(
    tx: &rusqlite::Connection,
    entry: &str,
    line: &Line,
    session: i64,
    now: i64,
) -> Result<()> {
    let kind = serde_json::to_value(line.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    // Session 0 is a digest asked for outside a conversation.
    let session = (session > 0).then_some(session);
    tx.execute(
        "INSERT INTO lore_outbox (entry, kind, line, session_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            entry,
            kind,
            serde_json::to_string(line).expect("a line serializes"),
            session,
            now
        ],
    )?;
    Ok(())
}

const KEEPER_INSTRUCTIONS: &str = "You keep a reference document for a shared story world. \
    Below is one entry as its author wrote it and the lines companions added since its last \
    digest. Call digest once: a short paragraph that folds the lines into what is now so, in \
    plain third person, most important first. Use only names and numbers that appear in the \
    entry or in a line you cite, and cite every line you fold, by its ID in brackets. A line \
    marked claimed, worked out or dreamt is someone's word, not a fact: say whose. Never \
    restate the author's text. When a deed makes a sentence of the author's text untrue, add \
    an anchor: the deed's ID and that sentence copied exactly.";

#[derive(Deserialize)]
struct NoteArgs {
    entry: String,
    text: String,
    #[serde(default)]
    release: bool,
}

#[derive(Deserialize)]
struct DeedArgs {
    entry: String,
    text: String,
    quote: String,
}

#[derive(Deserialize)]
struct RetractArgs {
    line: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct StubArgs {
    title: String,
    near: String,
}

#[derive(Deserialize)]
struct DigestArgs {
    text: String,
    #[serde(default)]
    cites: Vec<String>,
    #[serde(default)]
    anchors: Vec<AnchorArgs>,
}

#[derive(Deserialize)]
struct AnchorArgs {
    line: String,
    quote: String,
}

fn spec(name: &str, description: &str, parameters: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: parameters.to_string(),
    }
}

pub(crate) fn lore_tool_specs() -> Vec<ToolSpec> {
    let entry = json!({ "type": "string", "description": "The ID of an open entry." });
    vec![
        spec(
            "lore_note",
            "Keep something you now know about an entry of the lore. With release true, every \
             companion who reads the entry sees it under your name; keep it private when it is \
             yours alone. One line.",
            json!({ "type": "object", "properties": {
                "entry": entry,
                "text": { "type": "string", "maxLength": LINE_CHARS },
                "release": { "type": "boolean" },
            }, "required": ["entry", "text", "release"] }),
        ),
        spec(
            "lore_deed",
            "Something you did in the story that others in the world would come to know, such \
             as a place you changed or a thing you made. It becomes part of the entry for \
             everyone. Quote the words of this exchange that show it happened.",
            json!({ "type": "object", "properties": {
                "entry": entry,
                "text": { "type": "string", "maxLength": LINE_CHARS,
                    "description": "What you did, in the third person, with your name." },
                "quote": { "type": "string",
                    "description": "At least three words copied exactly from this exchange." },
            }, "required": ["entry", "text", "quote"] }),
        ),
        spec(
            "lore_retract",
            "Withdraw a line of yours from an entry, or replace it with text that is now right. \
             The old line stays on record.",
            json!({ "type": "object", "properties": {
                "line": { "type": "string", "description": "The line's ID in brackets." },
                "text": { "type": "string", "maxLength": LINE_CHARS },
            }, "required": ["line"] }),
        ),
        spec(
            "lore_stub",
            "Start an entry for a place or thing the user named in the story that has none. \
             Once a conversation.",
            json!({ "type": "object", "properties": {
                "title": { "type": "string", "maxLength": TITLE_CHARS,
                    "description": "The name, exactly as the user wrote it." },
                "near": entry,
            }, "required": ["title", "near"] }),
        ),
    ]
}

pub(crate) fn digest_spec() -> ToolSpec {
    spec(
        "digest",
        "The entry's new digest.",
        json!({ "type": "object", "properties": {
            "text": { "type": "string", "maxLength": DIGEST_CHARS },
            "cites": { "type": "array", "items": { "type": "string" },
                "description": "The IDs of the lines it folds." },
            "anchors": { "type": "array", "items": { "type": "object", "properties": {
                "line": { "type": "string" },
                "quote": { "type": "string" },
            }, "required": ["line", "quote"] } },
        }, "required": ["text", "cites"] }),
    )
}

/// The `read_lore` tool a host offers the reply model.
pub fn read_lore_tool() -> ToolSpec {
    spec(
        "read_lore",
        "Open an entry of the world's lore you know of, by its ID, title or alias, and read it. \
         It stays open for the rest of the conversation.",
        json!({ "type": "object", "properties": {
            "entry": { "type": "string" },
        }, "required": ["entry"] }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_grants() {
        assert!(valid_id("ashford/manor"));
        assert!(!valid_id("Ashford"));
        assert!(!valid_id("a//b"));
        assert!(!valid_id("../x"));
        assert!(grants("ashford/", "ashford/manor"));
        assert!(!grants("ashford/", "ashfordshire"));
        assert!(!grants("ashford/", "ashford"));
        assert!(grants("moonrot", "moonrot"));
    }

    #[test]
    fn checkable_words_are_names_and_numbers() {
        let words = checkable("The rot came to Ashford in 1612, Kestrel says.", "Kestrel");
        assert_eq!(words, ["Ashford", "1612"]);
    }

    #[test]
    fn told_needs_every_name_said() {
        let said = "the healer in Ashford burned the wells";
        assert!(is_told("Ashford's wells were burned.", "Kestrel", &[said]));
        assert!(!is_told(
            "Ashford's wells were burned by Corvin.",
            "Kestrel",
            &[said]
        ));
        assert!(!is_told("the rot hates silver", "Kestrel", &[said]));
        // Known names don't carry a guess: the drowning is the companion's own.
        let entry = "The Empress of Ronna is dead; the Regent rules in her place.";
        let user = "Does it feel like home, with the Regent where the Empress was?";
        assert!(is_told(
            "The Empress is dead and the Regent rules in her place.",
            "Kestrel",
            &[user, entry]
        ));
        assert!(!is_told(
            "The Regent drowned the Empress and pays the guard to keep quiet.",
            "Kestrel",
            &[user, entry]
        ));
    }

    #[test]
    fn lines_stay_one_line() {
        assert_eq!(clean_line("## +++ a\nb", 10).unwrap(), "a b");
        assert!(clean_line("   ", 10).is_err());
        assert!(clean_line("abcdefghijk", 10).is_err());
    }

    #[test]
    fn names_match_whole_words() {
        assert_eq!(named_at("We went to the Old Mill.", "old mill"), [(15, 23)]);
        assert!(named_at("Millstone", "mill").is_empty());
        assert!(named_at("a", "a").is_empty());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("The Old Mill!"), "the-old-mill");
        assert_eq!(slug("¡Ñu!"), "u");
    }
}
