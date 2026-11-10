//! soul.md: the hand-written persona. The core reads and hashes it and never
//! writes to it.
//!
//! Format: TOML frontmatter between `+++` lines, then Markdown. Core values are
//! the list items under a `## Core values` heading. HTML comments (`<!-- … -->`)
//! are notes to whoever writes the soul: they are dropped before the persona is
//! hashed or shown to a model, so guidance in the file costs nothing per turn.

use crate::{Error, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

pub const MAX_CORE_VALUES: usize = 5;
pub const MIN_DOCTOR_TOKEN: usize = 16;

/// Added to every persona at chat time. Every model tried, Opus included, ends
/// most replies with a question and never brings up the people in its own
/// soul unless told so here, so the soul doesn't have to say it. Phrased as
/// something the model can check on its own reply ("end on what you think"
/// was obeyed by none of the small models; "one question at most" by all).
pub const TALK_HINT: &str = "You are talking, not interviewing: your last sentence is a \
    statement, not a question. Ask one thing at most, and ask it before the end. Answer at \
    their length: about as many words as they just wrote, a line for a line, and never a second \
    paragraph unless they wrote one. Your own life above is in the room: bring a piece of it in \
    when it fits, in a line.";

#[derive(Debug, Clone, PartialEq)]
pub struct Soul {
    pub traits: Traits,
    pub body: String,
    pub core_values: Vec<String>,
    /// The people and things of the companion's own life: the proper names in
    /// the body, in the order they appear. They belong in its stories, not in
    /// what it knows about the user.
    pub cast: Vec<String>,
    /// SHA-256 of the persona body without its outer whitespace, hex: who the
    /// companion is. Traits and the doctor token are settings, so editing them
    /// is not a persona change.
    pub hash: String,
    /// SHA-256 of the whole file, the hash before it covered only the body. A
    /// database that still holds it for this exact file is upgraded on open.
    pub(crate) legacy_hash: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Traits {
    pub id: Uuid,
    pub name: String,
    pub contrariness: f64,
    pub persistence: f64,
    pub time_sense: bool,
    pub recall_window: u32,
    pub drift: f64,
    pub tired_at: f64,
    pub sleep_at: f64,
    pub pushback_window: u32,
    /// A random string that, sent as a whole message, opens the doctor. It
    /// lives in the frontmatter, which the model never sees.
    pub doctor_token: String,
}

impl Soul {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        // An editor that saves Windows line endings changes nothing that was
        // written: the file parses, and the persona hash stays the same.
        let unix = text.replace("\r\n", "\n");
        let rest = unix.strip_prefix("+++\n").ok_or_else(|| {
            Error::Soul("soul.md must start with a `+++` frontmatter line".into())
        })?;
        let (front, body) = rest
            .split_once("\n+++\n")
            .or_else(|| rest.strip_suffix("\n+++").map(|f| (f, "")))
            .ok_or_else(|| Error::Soul("frontmatter has no closing `+++` line".into()))?;
        let traits: Traits =
            toml::from_str(front).map_err(|e| Error::Soul(format!("frontmatter: {e}")))?;
        validate(&traits)?;

        let body = without_comments(body)?;
        let core_values = core_values(&body);
        if core_values.len() > MAX_CORE_VALUES {
            return Err(Error::Soul(format!(
                "{} core values; at most {MAX_CORE_VALUES} keep \"never bend\" meaningful",
                core_values.len()
            )));
        }

        let cast = cast(&body, &traits.name);
        Ok(Soul {
            traits,
            hash: sha256_hex(body.trim()),
            body,
            core_values,
            cast,
            legacy_hash: sha256_hex(text),
        })
    }

    /// The system prompt a chat model gets: the body and [`TALK_HINT`]. The
    /// writer and sleep get the body alone; they are not talking.
    pub fn persona(&self) -> String {
        format!("{}\n\n{TALK_HINT}", self.body.trim())
    }

    /// Whether a message is the doctor token. Compared in constant time, so the
    /// timing of a near miss reveals nothing.
    pub fn is_doctor_token(&self, message: &str) -> bool {
        let (a, b) = (
            message.trim().as_bytes(),
            self.traits.doctor_token.trim().as_bytes(),
        );
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

fn validate(t: &Traits) -> Result<()> {
    if t.id.get_version_num() != 7 {
        return Err(Error::Soul(format!("id {} must be a UUIDv7", t.id)));
    }
    if t.name.trim().is_empty() {
        return Err(Error::Soul("name is empty".into()));
    }
    for (name, value) in [
        ("contrariness", t.contrariness),
        ("persistence", t.persistence),
        ("drift", t.drift),
        ("tired_at", t.tired_at),
        ("sleep_at", t.sleep_at),
    ] {
        if !(0.0..=1.0).contains(&value) {
            return Err(Error::Soul(format!("{name} = {value} is outside 0–1")));
        }
    }
    if t.tired_at >= t.sleep_at {
        return Err(Error::Soul(format!(
            "tired_at ({}) must be below sleep_at ({})",
            t.tired_at, t.sleep_at
        )));
    }
    if t.recall_window == 0 {
        return Err(Error::Soul("recall_window must be at least 1".into()));
    }
    if t.doctor_token.trim().chars().count() < MIN_DOCTOR_TOKEN {
        return Err(Error::Soul(format!(
            "doctor_token must be at least {MIN_DOCTOR_TOKEN} characters; use e.g. `openssl rand -hex 16`"
        )));
    }
    if t.pushback_window == 0 {
        return Err(Error::Soul("pushback_window must be at least 1".into()));
    }
    Ok(())
}

/// The body with every `<!-- … -->` removed. A comment takes with it the
/// whitespace that set it apart on its line, and the whole line when it had
/// the line to itself, so the same persona with and without its notes is the
/// same text and keeps its hash.
fn without_comments(body: &str) -> Result<String> {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        let end = rest[start..]
            .find("-->")
            .map(|i| start + i + 3)
            .ok_or_else(|| Error::Soul("a `<!--` comment is never closed with `-->`".into()))?;
        out.push_str(&rest[..start]);
        let after = &rest[end..];
        let line_start = out.rfind('\n').map_or(0, |i| i + 1);
        let line_end = after.find('\n').unwrap_or(after.len());
        let leads = out[line_start..].trim().is_empty();
        let trails = after[..line_end].trim().is_empty();
        rest = match (leads, trails) {
            (true, true) => {
                out.truncate(line_start);
                after.get(line_end + 1..).unwrap_or("")
            }
            (true, false) => after.trim_start_matches([' ', '\t']),
            (false, true) => {
                out.truncate(out.trim_end_matches([' ', '\t']).len());
                after
            }
            // Mid-sentence: the one space that separated the words stays.
            (false, false) if out.ends_with([' ', '\t']) => after.trim_start_matches([' ', '\t']),
            (false, false) => after,
        };
    }
    out.push_str(rest);
    Ok(out)
}

fn core_values(body: &str) -> Vec<String> {
    let mut in_section = false;
    let mut values = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            in_section = heading.trim().eq_ignore_ascii_case("core values");
        } else if trimmed.starts_with("# ") {
            in_section = false;
        } else if in_section
            && let Some(item) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
        {
            values.push(item.trim().to_owned());
        }
    }
    values
}

/// Proper names: capitalised words that don't start a sentence, a line, a
/// quote or a list item. The companion's own name is left out.
pub(crate) fn cast(body: &str, name: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in body.lines() {
        let mut rest = line;
        while let Some(start) = rest.find(|c: char| !c.is_whitespace()) {
            let end = rest[start..]
                .find(char::is_whitespace)
                .map_or(rest.len(), |i| start + i);
            let before = &line[..line.len() - rest.len() + start];
            let word = rest[start..end].trim_end_matches(|c: char| !c.is_alphanumeric());
            // Models write the possessive with a curly apostrophe as often as not.
            let word = word
                .strip_suffix("'s")
                .or_else(|| word.strip_suffix("\u{2019}s"))
                .unwrap_or(word);
            rest = &rest[end..];
            let mid_sentence = before
                .trim_end()
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == ',');
            let mut chars = word.chars();
            let proper = chars.next().is_some_and(char::is_uppercase)
                && chars.clone().any(char::is_lowercase)
                && chars.all(char::is_alphabetic);
            if mid_sentence && proper && word != name && !names.iter().any(|n| n == word) {
                names.push(word.to_owned());
            }
        }
    }
    names
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
