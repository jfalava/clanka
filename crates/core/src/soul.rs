//! soul.md: the hand-written persona. The core reads and hashes it and never
//! writes to it.
//!
//! Format: TOML frontmatter between `+++` lines, then Markdown. Core values are
//! the list items under a `## Core values` heading.

use crate::{Error, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

pub const MAX_CORE_VALUES: usize = 5;
pub const MIN_DOCTOR_TOKEN: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub struct Soul {
    pub traits: Traits,
    pub body: String,
    pub core_values: Vec<String>,
    /// SHA-256 of the whole file, hex. Changes to traits or text both count.
    pub hash: String,
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
        let rest = text.strip_prefix("+++\n").ok_or_else(|| {
            Error::Soul("soul.md must start with a `+++` frontmatter line".into())
        })?;
        let (front, body) = rest
            .split_once("\n+++\n")
            .or_else(|| rest.strip_suffix("\n+++").map(|f| (f, "")))
            .ok_or_else(|| Error::Soul("frontmatter has no closing `+++` line".into()))?;
        let traits: Traits =
            toml::from_str(front).map_err(|e| Error::Soul(format!("frontmatter: {e}")))?;
        validate(&traits)?;

        let core_values = core_values(body);
        if core_values.len() > MAX_CORE_VALUES {
            return Err(Error::Soul(format!(
                "{} core values; at most {MAX_CORE_VALUES} keep \"never bend\" meaningful",
                core_values.len()
            )));
        }

        let hash = Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Soul {
            traits,
            body: body.to_owned(),
            core_values,
            hash,
        })
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
