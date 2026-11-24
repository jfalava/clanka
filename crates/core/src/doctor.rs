//! The doctor: the user's tools for when a companion's memory goes wrong.
//!
//! Every doctor action that writes requires the freeze to be on, so nothing
//! else changes the memory while the doctor works. Seared memories never change, not
//! even here: rollback leaves them and lists them, and re-interpretation
//! refuses them. Only the CLI delete removes one.

use crate::companion::{ensure_doctor, ensure_embedder, log, meta, set_meta};
use crate::deps::{ToolCall, ToolSpec};
use crate::diagnostics::Pass;
use crate::memory::{Rewrite, remove_memory, rewrite_interpretation};
use crate::model::{Level, Speaker};
use crate::writer::args;
use crate::{Companion, Error, Memory, Reflection, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::Instant;

/// Memories re-interpreted per model call during a persona update, and the
/// most a front end should hand [`Companion::reinterpret`] at once.
pub const REINTERPRET_BATCH: usize = 20;

/// One entry of the write log.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WriteEntry {
    pub id: i64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub target_id: Option<i64>,
    pub payload: Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Rollback {
    /// Memories written after the point, now gone.
    pub removed: Vec<i64>,
    /// Memories changed after the point, now as they were.
    pub restored: Vec<i64>,
    /// Threads closed after the point, open again.
    pub reopened: Vec<i64>,
    /// Seared memories written or seared after the point. They never change;
    /// delete them from the CLI if they shouldn't exist.
    pub kept_seared: Vec<i64>,
    /// Mood, turn note, soul section and pushback rows removed.
    pub state_rows: usize,
}

/// What `delete_all_memory` did. `failed` keeps the IDs it could not remove, so
/// one broken memory never hides the rest of the wipe.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteAll {
    /// Memories hard-deleted, including seared ones.
    pub memories: usize,
    /// Source turns no other memory cited, removed with them.
    pub turns: usize,
    /// Leftover turns nothing cited, and every session, removed afterwards.
    /// Both stay 0 when any memory failed.
    pub leftover_turns: usize,
    pub sessions: usize,
    /// Memories that refused to delete, with the reason.
    pub failed: Vec<(i64, String)>,
}

/// A fact that its source turns don't back up.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FactIssue {
    pub memory: i64,
    /// `None` when the memory has facts but cites no turns at all.
    pub fact: Option<(String, String)>,
}

impl Companion {
    pub fn is_archived(&self) -> Result<bool> {
        Ok(meta(&self.conn, "archived")?.as_deref() == Some("1"))
    }

    /// The fork's first half: this companion becomes read-only for good. Give
    /// soul.md a new UUIDv7 and the next open starts the companion anew; nothing here is deleted.
    pub fn archive(&mut self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        set_meta(&tx, "archived", "1")?;
        log(
            &tx,
            "cli",
            "archive",
            "companion",
            None,
            &json!({}),
            self.deps.clock.now_ms(),
        )?;
        Ok(tx.commit()?)
    }

    /// Every write after `since` (milliseconds since the epoch), oldest first.
    pub fn writes_since(&self, since: i64) -> Result<Vec<WriteEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, actor, action, target, target_id, payload, created_at FROM write_log
             WHERE created_at > ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([since], |r| {
            let payload: String = r.get(5)?;
            Ok(WriteEntry {
                id: r.get(0)?,
                actor: r.get(1)?,
                action: r.get(2)?,
                target: r.get(3)?,
                target_id: r.get(4)?,
                payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The ID of the newest entry in the write log, or 0 when it is empty. A
    /// front end that showed a plan sends it back with the action, to learn
    /// that the memory was written to in between.
    pub fn write_log_head(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT coalesce(max(id), 0) FROM write_log", [], |r| {
                r.get(0)
            })?)
    }

    /// What [`Companion::rollback_to`] would do, without doing it: its report
    /// for the memory as it is now. A read, so it does not ask for the freeze;
    /// without one the memory may change before the rollback runs.
    pub fn rollback_plan(&self, to: i64) -> Result<Rollback> {
        let mut report = undo(&self.conn, to)?.report();
        report.reopened = reopening(&self.conn, to)?;
        for table in ROLLED_BACK_STATE {
            let rows: i64 = self.conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE created_at > ?1"),
                [to],
                |r| r.get(0),
            )?;
            report.state_rows += rows as usize;
        }
        Ok(report)
    }

    /// Puts the companion's memory back as it was at `to`. The transcript and the write
    /// log are never touched, and seared memories are left as they are.
    pub fn rollback_to(&mut self, to: i64) -> Result<Rollback> {
        ensure_doctor(&self.conn)?;
        let undo = undo(&self.conn, to)?;
        let mut report = undo.report();
        // Embeddings first: the model never runs inside the write lock.
        let mut restore = Vec::new();
        for changed in &undo.changed {
            if changed.level == Level::Seared {
                continue;
            }
            let embedding = if changed.now == changed.then {
                None
            } else {
                Some(self.embed(&changed.then)?)
            };
            restore.push((changed, embedding));
        }

        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_embedder(&tx, self.deps.embedder.model_id())?;
        for &id in &report.removed {
            remove_memory(&tx, id, now)?;
        }
        for (changed, embedding) in restore {
            let id = changed.id;
            tx.execute(
                "DELETE FROM memory_versions WHERE memory_id = ?1 AND created_at > ?2",
                params![id, to],
            )?;
            tx.execute(
                "DELETE FROM drift_events WHERE memory_id = ?1 AND created_at > ?2",
                params![id, to],
            )?;
            tx.execute(
                "UPDATE memories SET interpretation = ?1, level = ?2, updated_at = ?3 WHERE id = ?4",
                params![changed.then, changed.then_level, now, id],
            )?;
            if let Some(embedding) = embedding {
                tx.execute("DELETE FROM memory_vectors WHERE memory_id = ?1", [id])?;
                tx.execute(
                    "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
                    params![id, embedding],
                )?;
            }
        }
        report.reopened = reopening(&tx, to)?;
        tx.execute(
            "UPDATE memories SET resolved_at = NULL, updated_at = ?1 WHERE resolved_at > ?2",
            params![now, to],
        )?;
        for table in ROLLED_BACK_STATE {
            report.state_rows +=
                tx.execute(&format!("DELETE FROM {table} WHERE created_at > ?1"), [to])?;
        }
        let summary = json!({
            "to": to, "removed": report.removed.len(), "restored": report.restored.len(),
            "reopened": report.reopened.len(), "kept_seared": report.kept_seared,
        });
        log(&tx, "doctor", "rollback", "companion", None, &summary, now)?;
        tx.commit()?;
        Ok(report)
    }

    /// Wipes every memory, seared ones included, then the transcript and the
    /// sessions, so the companion starts blank. Needs the freeze: nothing else
    /// may write between the deletes. Each memory goes through
    /// [`Companion::delete_memory`], newest first, so a memory that replaced
    /// another can be restored before its replacement is gone. A memory that
    /// fails is recorded and the rest still go, but then the transcript, state
    /// and sessions are kept; run it again once the failure is fixed.
    pub fn delete_all_memory(&mut self) -> Result<DeleteAll> {
        ensure_doctor(&self.conn)?;
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM memories ORDER BY id DESC")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut report = DeleteAll::default();
        for id in ids {
            match self.delete_memory(id) {
                Ok(deleted) => {
                    report.memories += 1;
                    report.turns += deleted.turns.len();
                }
                Err(e) => report.failed.push((id, e.to_string())),
            }
        }
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Another process may have unfrozen while the memories went; the sweep
        // would then take turns written since.
        ensure_doctor(&tx)?;
        // A memory that survived still cites its turns and may point at a
        // session, so the transcript stays until a later run removes it too.
        if report.failed.is_empty() {
            // The leftover turns (their notes and pushbacks cascade), then the
            // state, then the sessions: turns and mood states point at sessions,
            // and nothing cascades from one.
            report.leftover_turns = tx.execute("DELETE FROM turns", [])?;
            for table in ["mood_states", "soul_section_versions"] {
                tx.execute(&format!("DELETE FROM {table}"), [])?;
            }
            report.sessions = tx.execute("DELETE FROM sessions", [])?;
        }
        let summary = json!({
            "memories": report.memories, "turns": report.turns,
            "leftover_turns": report.leftover_turns, "sessions": report.sessions,
            "failed": report.failed.len(),
        });
        log(
            &tx,
            "doctor",
            "delete_all",
            "companion",
            None,
            &summary,
            now,
        )?;
        tx.commit()?;
        Ok(report)
    }

    /// Checks every active memory's facts against the turns it cites: each fact's
    /// value must appear in what was said. Read-only.
    pub fn check_facts(&self) -> Result<Vec<FactIssue>> {
        let ids: Vec<i64> = self
            .conn
            .prepare(
                "SELECT id FROM memories WHERE status = 'active' AND facts != '{}' ORDER BY id",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut issues = Vec::new();
        for id in ids {
            let memory = self.memory(id)?;
            if memory.source_turns.is_empty() {
                issues.push(FactIssue {
                    memory: id,
                    fact: None,
                });
                continue;
            }
            let said = self.source_text(&memory, false)?;
            for (key, value) in &memory.facts {
                let value = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                if !crate::writer::said_verbatim(&said, &value) {
                    issues.push(FactIssue {
                        memory: id,
                        fact: Some((key.clone(), value)),
                    });
                }
            }
        }
        Ok(issues)
    }

    /// Asks the companion to re-read what was really said and write how it sees
    /// each of these memories now, under its current persona. Seared memories and
    /// memories without source turns are refused before the model is called.
    pub fn reinterpret(&mut self, ids: &[i64]) -> Result<Reflection> {
        ensure_doctor(&self.conn)?;
        if meta(&self.conn, "persona_hash")?.as_deref() != Some(self.soul.hash.as_str()) {
            return Err(Error::PersonaUnresolved);
        }
        let mut listed = Vec::new();
        for &id in ids {
            let memory = self.memory(id)?;
            if memory.level == Level::Seared {
                return Err(Error::Seared(id));
            }
            if memory.source_turns.is_empty() {
                return Err(Error::Invalid(format!(
                    "memory {id} cites no turns to re-read"
                )));
            }
            listed.push(format!(
                "#{id} [{}, {}] {}\nWhat was said:\n{}",
                memory.kind.as_str(),
                memory.level.as_str(),
                memory.interpretation,
                self.source_text(&memory, true)?
            ));
        }
        let allowed: BTreeSet<i64> = ids.iter().copied().collect();
        let mut done = BTreeSet::new();
        let system = format!(
            "{}\n\n{}",
            self.soul.body.trim(),
            instructions(&self.soul.traits.name)
        );
        let started = Instant::now();
        let result = self.tool_loop(
            Pass::Reinterpret,
            system,
            listed.join("\n\n"),
            tool_specs(),
            crate::actions::MAX_ACTIONS,
            &[],
            |c, call| c.run_reinterpret(call, &allowed, &mut done),
        );
        self.diagnose_pass(
            Pass::Reinterpret,
            started,
            false,
            result.as_ref().ok(),
            result.as_ref().err(),
        );
        result
    }

    /// "Update" after a persona change: accept the new persona and re-interpret
    /// every memory that can be, in batches. Freezes writes while it works and
    /// unfreezes when it succeeds; on failure it stays frozen for you to look.
    /// A freeze that was on before, the doctor's or a failed update's, stays on.
    /// `progress` hears how many memories are done, of how many, before the
    /// first batch and after each.
    pub fn update_persona(
        &mut self,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<Reflection>> {
        let was_frozen = self.is_frozen()?;
        self.set_frozen(true)?;
        self.continue_with_persona()?;
        let ids: Vec<i64> = self
            .conn
            .prepare(
                "SELECT id FROM memories m WHERE status = 'active' AND level != 'seared'
                 AND EXISTS (SELECT 1 FROM memory_sources s WHERE s.memory_id = m.id) ORDER BY id",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut reflections = Vec::new();
        let mut done = 0;
        progress(done, ids.len());
        for batch in ids.chunks(REINTERPRET_BATCH) {
            reflections.push(self.reinterpret(batch)?);
            done += batch.len();
            progress(done, ids.len());
        }
        if !was_frozen {
            self.set_frozen(false)?;
        }
        Ok(reflections)
    }

    /// What the turns a memory cites say. `heard` leaves out what was said out
    /// of the companion's earshot, for a model that must not learn it; a fact
    /// check reads it all, since an echo can reveal what was said there.
    pub(crate) fn source_text(&self, memory: &Memory, heard: bool) -> Result<String> {
        let lines = self.source_turns(memory.id)?.into_iter().map(|turn| {
            let content = if heard {
                crate::memory::heard(&turn.content)
            } else {
                turn.content
            };
            let who = match turn.speaker {
                Speaker::User => "user",
                Speaker::Companion => "you",
            };
            format!("({who}) {content}")
        });
        Ok(lines.collect::<Vec<_>>().join("\n"))
    }

    fn run_reinterpret(
        &mut self,
        call: &ToolCall,
        allowed: &BTreeSet<i64>,
        done: &mut BTreeSet<i64>,
    ) -> Result<String> {
        if call.name != "reinterpret" {
            return Err(Error::Invalid(format!(
                "there is no tool called {:?}",
                call.name
            )));
        }
        let a: Reinterpret = args(call)?;
        if !allowed.contains(&a.memory_id) {
            return Err(Error::Invalid(format!(
                "memory {} isn't one of the memories to re-read",
                a.memory_id
            )));
        }
        if a.interpretation.trim().is_empty() {
            return Err(Error::Invalid("interpretation is empty".into()));
        }
        if !done.insert(a.memory_id) {
            return Err(Error::Invalid(format!(
                "memory {} is already re-interpreted",
                a.memory_id
            )));
        }
        let embedding = self.embed(&a.interpretation)?;
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_doctor(&tx)?;
        ensure_embedder(&tx, self.deps.embedder.model_id())?;
        let level: Level = tx
            .query_row(
                "SELECT level FROM memories WHERE id = ?1",
                [a.memory_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound(a.memory_id))?;
        if level == Level::Seared {
            return Err(Error::Seared(a.memory_id));
        }
        let rewrite = Rewrite {
            id: a.memory_id,
            interpretation: &a.interpretation,
            embedding: &embedding,
            reason: "reinterpreted",
            drift_event: None,
        };
        rewrite_interpretation(&tx, &rewrite, &self.soul.hash, now)?;
        log(
            &tx,
            "doctor",
            "reinterpret",
            "memory",
            Some(a.memory_id),
            &json!({}),
            now,
        )?;
        tx.commit()?;
        Ok(format!("memory {} re-interpreted", a.memory_id))
    }
}

/// The tables a rollback drops the later rows of: the mood, the turn notes,
/// the soul sections and the pushbacks.
const ROLLED_BACK_STATE: [&str; 4] = [
    "mood_states",
    "turn_notes",
    "soul_section_versions",
    "pushbacks",
];

/// A memory from before the point of a rollback that changed after it.
struct Changed {
    id: i64,
    level: Level,
    /// Its text now, and in its last version from before the point.
    now: String,
    then: String,
    then_level: Level,
}

/// What a rollback finds to undo. [`Companion::rollback_plan`] reports it
/// and [`Companion::rollback_to`] acts on it, so the two cannot disagree.
struct Undo {
    /// Memories written after the point, newest first, with their level.
    born: Vec<(i64, Level)>,
    changed: Vec<Changed>,
}

fn undo(conn: &Connection, to: i64) -> Result<Undo> {
    let born = conn
        .prepare("SELECT id, level FROM memories WHERE created_at > ?1 ORDER BY id DESC")?
        .query_map([to], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // Memories from before the point that changed after it, with their last earlier version.
    let changed = conn
        .prepare(
            "SELECT m.id, m.level, m.interpretation, v.interpretation, v.level
             FROM memories m JOIN memory_versions v ON v.id =
               (SELECT max(id) FROM memory_versions WHERE memory_id = m.id AND created_at <= ?1)
             WHERE m.created_at <= ?1
               AND EXISTS (SELECT 1 FROM memory_versions WHERE memory_id = m.id AND created_at > ?1)
             ORDER BY m.id",
        )?
        .query_map([to], |r| {
            Ok(Changed {
                id: r.get(0)?,
                level: r.get(1)?,
                now: r.get(2)?,
                then: r.get(3)?,
                then_level: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Undo { born, changed })
}

impl Undo {
    /// Which memories go, which get their earlier text back and which are
    /// seared and stay: the report, but for what only the database can count.
    fn report(&self) -> Rollback {
        let mut report = Rollback::default();
        for &(id, level) in &self.born {
            if level == Level::Seared {
                report.kept_seared.push(id);
            } else {
                report.removed.push(id);
            }
        }
        for changed in &self.changed {
            if changed.level == Level::Seared {
                report.kept_seared.push(changed.id);
            } else {
                report.restored.push(changed.id);
            }
        }
        report.kept_seared.sort_unstable();
        report
    }
}

/// Threads closed after the point, which a rollback opens again. A thread
/// the rollback removes is not one of them, before it is removed or after.
fn reopening(conn: &Connection, to: i64) -> Result<Vec<i64>> {
    let ids = conn
        .prepare(
            "SELECT id FROM memories
             WHERE resolved_at > ?1 AND NOT (created_at > ?1 AND level != 'seared')
             ORDER BY id",
        )?
        .query_map([to], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

fn instructions(name: &str) -> String {
    format!(
        "You are {name}. Something went wrong with how you remember some things, so you are \
         re-reading what was really said. For each memory listed, call reinterpret with the \
         memory as you would keep it now: in first person, in one or two plain sentences, what \
         happened or what they told you and how you felt about it in a few words. Keep to what \
         was said: their feelings and reasons are the ones they said, and the memory doesn't \
         say what it meant. Make \
         every call in this one reply; you hear back only if one fails."
    )
}

#[derive(Deserialize)]
struct Reinterpret {
    memory_id: i64,
    interpretation: String,
}

pub(crate) fn tool_specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "reinterpret".into(),
        description: "How you see one of the listed memories now.".into(),
        parameters: json!({ "type": "object", "properties": {
            "memory_id": { "type": "integer" },
            "interpretation": { "type": "string" },
        }, "required": ["memory_id", "interpretation"] })
        .to_string(),
    }]
}
