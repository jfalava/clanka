//! `/triage`: what clanka did lately for one companion, for the person in the
//! chat to read or paste into a bug report: its state now, and its last
//! events from the diagnostics log.
//!
//! The report is made only of what the log already holds (numbers, yes/no
//! and words clanka spells out, see [`clanka_core::diagnostics`]) and of
//! yes/no state. Never anything said or remembered: it shows in group chats,
//! a platform keeps it, and it is meant to be pasted where anyone can read it.

use clanka_core::Companion;
use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;

/// Most events a report shows: about the last three exchanges.
pub const EVENTS: usize = 15;

/// What a log line carries beside its event that the report leaves out: the
/// event's kind and time head the row, and the companion is this one.
const SKIP: [&str; 6] = ["at", "event", "pass", "companion", "pid", "version"];

/// The report for `c`. `log` is the diagnostics log, or `None` when
/// `[diagnostics] enabled` is off.
pub fn report(c: &Companion, log: Option<&Path>) -> clanka_core::Result<String> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "clanka {} triage for {}",
        env!("CARGO_PKG_VERSION"),
        c.soul().traits.name
    );
    let state = if c.is_frozen()? {
        "frozen by the doctor"
    } else if c.dreaming()?.is_some() {
        "in a dream"
    } else if c.session_in_progress()?.is_some() {
        "awake, in a conversation"
    } else {
        "no conversation open"
    };
    let waiting = c.unconsolidated()?.len();
    let _ = match waiting {
        0 => writeln!(out, "Now: {state}."),
        1 => writeln!(out, "Now: {state}; 1 night waits to be slept on."),
        n => writeln!(out, "Now: {state}; {n} nights wait to be slept on."),
    };
    let Some(log) = log else {
        out.push_str(
            "Diagnostics are off ([diagnostics] enabled = false), so there are no events.",
        );
        return Ok(out);
    };
    let id = c.id().simple().to_string();
    let events = last_events(log, &id[id.len() - 8..], EVENTS);
    if events.is_empty() {
        out.push_str("No events for this companion yet.");
        return Ok(out);
    }
    let _ = writeln!(out, "Last {} events, oldest first (UTC):", events.len());
    for event in &events {
        let _ = writeln!(out, "{}", row(event));
    }
    out.pop();
    Ok(out)
}

/// The last `n` events in the log (and the full one before it) about the
/// companion whose ID ends in `companion`. Lines that can't be read are skipped.
fn last_events(log: &Path, companion: &str, n: usize) -> Vec<Value> {
    let mut events = Vec::new();
    for file in [log.with_extension("1.jsonl"), log.to_owned()] {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        events.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|event| event["companion"] == companion),
        );
    }
    let skip = events.len().saturating_sub(n);
    events.split_off(skip)
}

/// One event as `HH:MM:SS kind field…`: yes as the field's name, numbers and
/// words as `name=value`, no and 0 left out.
fn row(event: &Value) -> String {
    let at = event["at"].as_i64().unwrap_or(0);
    let secs = at.div_euclid(1000).rem_euclid(24 * 60 * 60);
    let mut kind = event["event"].as_str().unwrap_or("?").to_owned();
    if let Some(pass) = event["pass"].as_str() {
        kind = format!("{kind}.{pass}");
    }
    let mut row = format!(
        "{:02}:{:02}:{:02} {kind}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    if let Some(fields) = event.as_object() {
        for (name, value) in fields {
            if !SKIP.contains(&name.as_str()) {
                field(&mut row, name, value);
            }
        }
    }
    row
}

fn field(row: &mut String, name: &str, value: &Value) {
    match value {
        Value::Bool(true) => {
            let _ = write!(row, " {name}");
        }
        Value::Number(n) if n.as_f64() != Some(0.0) => {
            let _ = write!(row, " {name}={n}");
        }
        Value::String(word) => {
            let _ = write!(row, " {name}={word}");
        }
        Value::Array(words) if !words.is_empty() => {
            let words: Vec<&str> = words.iter().filter_map(Value::as_str).collect();
            let _ = write!(row, " {name}={}", words.join(","));
        }
        Value::Object(inner) => {
            for (part, value) in inner {
                field(row, &format!("{name}.{part}"), value);
            }
        }
        _ => {}
    }
}
