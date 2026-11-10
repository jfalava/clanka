//! The small state injected on every turn: mood, the note the companion leaves itself
//! between turns, and the editable soul sections. Every change is a new row, so
//! history is kept and the doctor can roll back.

use crate::companion::{ensure_writable, log};
use crate::model::{Actor, Section};
use crate::{Companion, Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::json;

/// Share of the pushback window the companion may spend pushing back at `contrariness`
/// 1: a soul at 0.6 with a 20-turn window gets 3 pushbacks per 20 turns.
pub const PUSHBACK_SHARE: f64 = 0.25;

/// Longest an editable soul section may be, in characters. Sections go into
/// every turn's context, so they must stay small.
pub const SECTION_MAX_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq)]
pub struct Mood {
    /// Good or bad mood, −1 to 1.
    pub valence: f64,
    /// Tired or lively, −1 to 1.
    pub energy: f64,
    /// How the companion feels about the user right now, −1 to 1.
    pub warmth: f64,
    /// One line on why.
    pub cause: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SectionVersion {
    pub id: i64,
    pub content: String,
    pub persona_hash: String,
    pub created_at: i64,
}

impl Companion {
    pub fn set_mood(&mut self, actor: Actor, mood: &Mood) -> Result<()> {
        for (axis, value) in [
            ("valence", mood.valence),
            ("energy", mood.energy),
            ("warmth", mood.warmth),
        ] {
            if !(-1.0..=1.0).contains(&value) {
                return Err(Error::Invalid(format!("{axis} = {value} is outside −1–1")));
            }
        }
        if mood.cause.trim().is_empty() {
            return Err(Error::Invalid("a mood needs a cause".into()));
        }
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        tx.execute(
            "INSERT INTO mood_states (session_id, valence, energy, warmth, cause, created_at)
             VALUES ((SELECT id FROM sessions WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1),
                     ?1, ?2, ?3, ?4, ?5)",
            params![mood.valence, mood.energy, mood.warmth, mood.cause, now],
        )?;
        let id = tx.last_insert_rowid();
        log(
            &tx,
            actor.as_str(),
            "set_mood",
            "mood",
            Some(id),
            &json!({}),
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The latest mood, or `None` before the first one.
    pub fn mood(&self) -> Result<Option<Mood>> {
        Ok(self
            .conn
            .query_row(
                "SELECT valence, energy, warmth, cause FROM mood_states ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok(Mood {
                        valence: r.get(0)?,
                        energy: r.get(1)?,
                        warmth: r.get(2)?,
                        cause: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// The note the writer leaves after a turn, read into the next turn's context.
    pub fn write_turn_note(&mut self, actor: Actor, turn: i64, note: &str) -> Result<()> {
        if note.trim().is_empty() {
            return Err(Error::Invalid("turn note is empty".into()));
        }
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        ensure_turn(&tx, turn)?;
        tx.execute(
            "INSERT INTO turn_notes (turn_id, note, created_at) VALUES (?1, ?2, ?3)",
            params![turn, note, now],
        )?;
        let id = tx.last_insert_rowid();
        log(
            &tx,
            actor.as_str(),
            "write_turn_note",
            "turn_note",
            Some(id),
            &json!({ "turn": turn }),
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The most recent note and the turn it follows.
    pub fn last_turn_note(&self) -> Result<Option<(i64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT turn_id, note FROM turn_notes ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Replaces a section's content with a new version; earlier versions stay.
    /// The CLI rolls back by writing an earlier version's content again.
    pub fn write_section(&mut self, actor: Actor, section: Section, content: &str) -> Result<()> {
        let chars = content.chars().count();
        if chars > SECTION_MAX_CHARS {
            return Err(Error::Invalid(format!(
                "section {} is {chars} characters; the limit is {SECTION_MAX_CHARS}",
                section.as_str()
            )));
        }
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        tx.execute(
            "INSERT INTO soul_section_versions (name, content, persona_hash, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![section, content, self.soul.hash, now],
        )?;
        let id = tx.last_insert_rowid();
        let summary = json!({ "section": section.as_str(), "chars": chars });
        log(
            &tx,
            actor.as_str(),
            "write_section",
            "soul_section",
            Some(id),
            &summary,
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The current content, or `None` if the section was never written.
    pub fn section(&self, section: Section) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT content FROM soul_section_versions WHERE name = ?1 ORDER BY id DESC LIMIT 1",
                [section],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Every version, oldest first.
    pub fn section_history(&self, section: Section) -> Result<Vec<SectionVersion>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, persona_hash, created_at FROM soul_section_versions
             WHERE name = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([section], |r| {
            Ok(SectionVersion {
                id: r.get(0)?,
                content: r.get(1)?,
                persona_hash: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

impl Companion {
    /// Records that the companion pushed back on the user in `turn`. A clash with a core value
    /// is kept but never counts against the budget.
    pub fn record_pushback(
        &mut self,
        actor: Actor,
        turn: i64,
        core_value: bool,
        about: &str,
    ) -> Result<()> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        ensure_turn(&tx, turn)?;
        tx.execute(
            "INSERT INTO pushbacks (turn_id, core_value, about, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![turn, core_value, about, now],
        )?;
        let id = tx.last_insert_rowid();
        log(
            &tx,
            actor.as_str(),
            "pushback",
            "pushback",
            Some(id),
            &json!({ "core_value": core_value }),
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Pushbacks over the last `pushback_window` turns that count, and the
    /// limit `contrariness` allows. At or over the limit, the companion lets the next one go.
    pub fn pushback_pressure(&self) -> Result<(usize, usize)> {
        let traits = &self.soul.traits;
        let count: i64 = self.conn.query_row(
            // Dream turns aren't counted: nothing pushes back in a dream.
            "SELECT count(*) FROM pushbacks WHERE core_value = 0 AND turn_id >=
               (SELECT coalesce(min(id), 0) FROM (SELECT t.id FROM turns t
                  JOIN sessions s ON s.id = t.session_id WHERE s.dream = 0
                  ORDER BY t.id DESC LIMIT ?1))",
            [traits.pushback_window],
            |r| r.get(0),
        )?;
        let limit =
            (traits.contrariness * f64::from(traits.pushback_window) * PUSHBACK_SHARE).round();
        Ok((count as usize, limit as usize))
    }
}

fn ensure_turn(conn: &Connection, turn: i64) -> Result<()> {
    let found = conn
        .query_row("SELECT 1 FROM turns WHERE id = ?1", [turn], |_| Ok(()))
        .optional()?;
    found.ok_or_else(|| Error::Invalid(format!("turn {turn} does not exist")))
}
