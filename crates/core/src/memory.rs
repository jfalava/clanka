//! The write path: sessions, the immutable transcript and memories.
//!
//! Every write checks the doctor's freeze and the accepted persona inside its
//! own IMMEDIATE transaction, so a freeze from another process applies to the
//! very next write. Embeddings are computed before the transaction opens, so
//! other processes never wait on the model.

use crate::companion::{ensure_embedder, ensure_writable, log};
use crate::model::{Actor, Level, MemoryType, SessionEnd, Speaker, Status, Urgency};
use crate::{Companion, Error, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Map, Value, json};

/// Cosine similarity at or above which a new memory counts as a copy of an
/// active memory of the same type. MiniLM scores run high; tune on real data.
pub const DUPLICATE_SIMILARITY: f32 = 0.92;

/// What the writer (or the CLI) wants to remember.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryDraft {
    pub kind: MemoryType,
    pub level: Level,
    /// Checkable facts (names, dates, places). Immutable once written.
    pub facts: Map<String, Value>,
    pub interpretation: String,
    /// Turns that anchor the facts. Required whenever `facts` is not empty.
    pub source_turns: Vec<i64>,
    pub occurred_at: Option<i64>,
    /// Opinions only: how strongly the companion holds the stance, 0–1.
    pub strength: Option<f64>,
    /// Open threads only.
    pub urgency: Option<Urgency>,
    /// How the memory feels to the companion, −1 to 1. `None` is neutral.
    pub valence: Option<f64>,
}

impl MemoryDraft {
    pub fn new(kind: MemoryType, level: Level, interpretation: impl Into<String>) -> Self {
        Self {
            kind,
            level,
            facts: Map::new(),
            interpretation: interpretation.into(),
            source_turns: Vec::new(),
            occurred_at: None,
            strength: None,
            urgency: None,
            valence: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    pub id: i64,
    pub kind: MemoryType,
    pub level: Level,
    pub facts: Map<String, Value>,
    pub interpretation: String,
    pub status: Status,
    pub superseded_by: Option<i64>,
    pub occurred_at: Option<i64>,
    pub persona_hash: String,
    pub strength: Option<f64>,
    pub urgency: Option<Urgency>,
    pub valence: Option<f64>,
    pub recall_count: i64,
    pub last_recalled_session: Option<i64>,
    pub source_turns: Vec<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What a hard delete removed besides the memory itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleted {
    /// Source turns no other memory cited, now gone from the transcript.
    pub turns: Vec<i64>,
    /// The memory this one had superseded, which is current again.
    pub restored: Option<i64>,
}

impl Companion {
    /// The open session, or a new one if the last one ended. Sessions end only
    /// at sleep or by the doctor, so every process shares the same open session.
    pub fn open_session(&mut self) -> Result<i64> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        let open: Option<i64> = tx
            .query_row(
                "SELECT id FROM sessions WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let id = match open {
            Some(id) => id,
            None => {
                tx.execute("INSERT INTO sessions (started_at) VALUES (?1)", [now])?;
                tx.last_insert_rowid()
            }
        };
        tx.commit()?;
        Ok(id)
    }

    /// The open session, if something was said in it: what sleep would end.
    /// `None` when the companion is already asleep or nothing has happened yet,
    /// so a host that sleeps at its own conversation boundaries never calls
    /// the model over an empty night.
    pub fn session_in_progress(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT s.id FROM sessions s WHERE s.ended_at IS NULL
                 AND EXISTS (SELECT 1 FROM turns WHERE session_id = s.id)
                 ORDER BY s.id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Not gated by the freeze: the doctor ends sessions while writes are frozen.
    pub fn end_session(&mut self, session: i64, by: SessionEnd) -> Result<()> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_open(&tx, session)?;
        tx.execute(
            "UPDATE sessions SET ended_at = ?1, ended_by = ?2 WHERE id = ?3",
            params![now, by, session],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Appends to the immutable transcript.
    pub fn record_turn(&mut self, session: i64, speaker: Speaker, content: &str) -> Result<i64> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        ensure_open(&tx, session)?;
        tx.execute(
            "INSERT INTO turns (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![session, speaker, content, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Writes a new memory with its version, sources, vector and log entry, all
    /// or nothing. Refuses a near-duplicate of an active memory of the same type.
    pub fn write(&mut self, actor: Actor, draft: &MemoryDraft) -> Result<i64> {
        validate(draft)?;
        let embedding = self.embed(&draft.interpretation)?;
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        ensure_embedder(&tx, self.deps.embedder.model_id())?;
        let id = insert(
            &tx,
            actor,
            draft,
            &embedding,
            &self.soul.hash,
            now,
            None,
            self.duplicate_similarity,
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Replaces an active memory with a new one of the same type. The old one
    /// keeps its exact text and becomes the past. This is the only way facts
    /// change, and it works on seared memories too, since nothing in them is edited.
    pub fn supersede(&mut self, actor: Actor, old: i64, draft: &MemoryDraft) -> Result<i64> {
        validate(draft)?;
        let embedding = self.embed(&draft.interpretation)?;
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        ensure_embedder(&tx, self.deps.embedder.model_id())?;
        let (kind, status): (MemoryType, Status) = tx
            .query_row(
                "SELECT type, status FROM memories WHERE id = ?1",
                [old],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound(old))?;
        if status == Status::Superseded {
            return Err(Error::Invalid(format!(
                "memory {old} is already superseded"
            )));
        }
        if kind != draft.kind {
            return Err(Error::Invalid(format!(
                "memory {old} is a {}, not a {}",
                kind.as_str(),
                draft.kind.as_str()
            )));
        }
        let new = insert(
            &tx,
            actor,
            draft,
            &embedding,
            &self.soul.hash,
            now,
            Some(old),
            self.duplicate_similarity,
        )?;
        tx.execute(
            "UPDATE memories SET status = 'superseded', superseded_by = ?1, updated_at = ?2 WHERE id = ?3",
            params![new, now, old],
        )?;
        log(
            &tx,
            actor.as_str(),
            "supersede",
            "memory",
            Some(old),
            &json!({ "by": new }),
            now,
        )?;
        tx.commit()?;
        Ok(new)
    }

    /// The companion can only raise a level; the CLI can set any level. Nobody
    /// changes a seared memory.
    pub fn set_level(&mut self, actor: Actor, id: i64, level: Level) -> Result<()> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        let (current, interpretation): (Level, String) = tx
            .query_row(
                "SELECT level, interpretation FROM memories WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound(id))?;
        if current == Level::Seared {
            return Err(Error::Seared(id));
        }
        if level == current {
            return Ok(());
        }
        if actor != Actor::Cli && level < current {
            return Err(Error::LevelDown {
                current,
                requested: level,
            });
        }
        tx.execute(
            "UPDATE memories SET level = ?1, updated_at = ?2 WHERE id = ?3",
            params![level, now, id],
        )?;
        let reason = match actor {
            Actor::Writer | Actor::Sleep => "escalated",
            Actor::Cli => "cli",
        };
        tx.execute(
            "INSERT INTO memory_versions (memory_id, interpretation, level, persona_hash, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, interpretation, level, self.soul.hash, reason, now],
        )?;
        let change = json!({ "from": current.as_str(), "to": level.as_str() });
        log(
            &tx,
            actor.as_str(),
            "set_level",
            "memory",
            Some(id),
            &change,
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Closes an open thread. It stays a memory; it just stops chasing the user.
    pub fn resolve_thread(&mut self, actor: Actor, id: i64) -> Result<()> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        let (kind, status, resolved): (MemoryType, Status, Option<i64>) = tx
            .query_row(
                "SELECT type, status, resolved_at FROM memories WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or(Error::NotFound(id))?;
        if kind != MemoryType::OpenThread {
            return Err(Error::Invalid(format!(
                "memory {id} is a {}, not an open thread",
                kind.as_str()
            )));
        }
        if status == Status::Superseded || resolved.is_some() {
            return Err(Error::Invalid(format!(
                "thread {id} is already closed or superseded"
            )));
        }
        tx.execute(
            "UPDATE memories SET resolved_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        log(
            &tx,
            actor.as_str(),
            "resolve_thread",
            "memory",
            Some(id),
            &json!({}),
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Rewrites a memory's interpretation as it drifts in the companion's sleep, with the
    /// seed and magnitude that set how far, so the doctor can explain it.
    /// Facts never drift and seared memories never change.
    pub(crate) fn drift_memory(
        &mut self,
        session: i64,
        id: i64,
        interpretation: &str,
        seed: u64,
        magnitude: f64,
    ) -> Result<()> {
        if interpretation.trim().is_empty() {
            return Err(Error::Invalid("interpretation is empty".into()));
        }
        let embedding = self.embed(interpretation)?;
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        ensure_embedder(&tx, self.deps.embedder.model_id())?;
        let (level, status): (Level, Status) = tx
            .query_row(
                "SELECT level, status FROM memories WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound(id))?;
        if level == Level::Seared {
            return Err(Error::Seared(id));
        }
        if status == Status::Superseded {
            return Err(Error::Invalid(format!("memory {id} is superseded")));
        }
        tx.execute(
            "INSERT INTO drift_events (session_id, memory_id, seed, magnitude, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session, id, seed.to_string(), magnitude, now],
        )?;
        let event = tx.last_insert_rowid();
        let rewrite = Rewrite {
            id,
            interpretation,
            embedding: &embedding,
            reason: "drift",
            drift_event: Some(event),
        };
        rewrite_interpretation(&tx, &rewrite, &self.soul.hash, now)?;
        let summary = json!({ "magnitude": magnitude, "drift_event": event });
        log(
            &tx,
            Actor::Sleep.as_str(),
            "drift",
            "memory",
            Some(id),
            &summary,
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The CLI's hard delete. Removes the memory, its versions, drift events and
    /// vector, and the turns it cited that no other memory cites. Works on seared
    /// memories and while the doctor's freeze is on: deleting is yours alone.
    /// If the memory had replaced another, that one is current again.
    pub fn delete_memory(&mut self, id: i64) -> Result<Deleted> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found = tx
            .query_row("SELECT 1 FROM memories WHERE id = ?1", [id], |_| Ok(()))
            .optional()?;
        found.ok_or(Error::NotFound(id))?;

        let own_turns: Vec<i64> = tx
            .prepare(
                "SELECT s.turn_id FROM memory_sources s WHERE s.memory_id = ?1
                 AND NOT EXISTS (SELECT 1 FROM memory_sources o
                                 WHERE o.turn_id = s.turn_id AND o.memory_id != ?1)",
            )?
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let restored = remove_memory(&tx, id, now)?;
        for turn in &own_turns {
            tx.execute("DELETE FROM turns WHERE id = ?1", [turn])?;
        }
        // The log keeps that a delete happened, never what was deleted.
        let report = json!({ "turns_deleted": own_turns.len(), "restored": restored });
        log(
            &tx,
            Actor::Cli.as_str(),
            "delete",
            "memory",
            Some(id),
            &report,
            now,
        )?;
        tx.commit()?;
        Ok(Deleted {
            turns: own_turns,
            restored,
        })
    }

    /// Everything the companion currently holds, oldest first: active memories,
    /// open threads included.
    pub fn active_memories(&self) -> Result<Vec<Memory>> {
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM memories WHERE status = 'active' ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.into_iter().map(|id| self.memory(id)).collect()
    }

    pub fn memory(&self, id: i64) -> Result<Memory> {
        let (mut memory, facts) = self
            .conn
            .query_row(
                "SELECT id, type, level, facts, interpretation, status, superseded_by, occurred_at,
                        persona_hash, strength, urgency, created_at, updated_at, valence,
                        recall_count, last_recalled_session
                 FROM memories WHERE id = ?1",
                [id],
                |r| {
                    let memory = Memory {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        level: r.get(2)?,
                        facts: Map::new(),
                        interpretation: r.get(4)?,
                        status: r.get(5)?,
                        superseded_by: r.get(6)?,
                        occurred_at: r.get(7)?,
                        persona_hash: r.get(8)?,
                        strength: r.get(9)?,
                        urgency: r.get(10)?,
                        valence: r.get(13)?,
                        recall_count: r.get(14)?,
                        last_recalled_session: r.get(15)?,
                        source_turns: Vec::new(),
                        created_at: r.get(11)?,
                        updated_at: r.get(12)?,
                    };
                    Ok((memory, r.get::<_, String>(3)?))
                },
            )
            .optional()?
            .ok_or(Error::NotFound(id))?;
        memory.facts = serde_json::from_str(&facts)
            .map_err(|e| Error::Invalid(format!("memory {id} facts: {e}")))?;
        let mut sources = self
            .conn
            .prepare("SELECT turn_id FROM memory_sources WHERE memory_id = ?1 ORDER BY turn_id")?;
        memory.source_turns = sources
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(memory)
    }

    /// One vector as sqlite-vec's little-endian f32 blob.
    ///
    /// Refuses when the stored vectors no longer come from this handle's
    /// model: the doctor re-embedded from another process since it opened, and
    /// a vector from the old model would sit among the new ones as noise.
    pub(crate) fn embed(&self, text: &str) -> Result<Vec<u8>> {
        let current = self.deps.embedder.model_id();
        if let Some(stored) = crate::companion::meta(&self.conn, "embedder_model")?
            && stored != current
        {
            return Err(Error::EmbedderChanged {
                stored,
                current: current.to_owned(),
            });
        }
        let embedder = &self.deps.embedder;
        let vector = embedder
            .embed(&[text])?
            .pop()
            .ok_or_else(|| Error::Embedder("expected one vector, got none".into()))?;
        crate::companion::blob(&vector, embedder.dimensions())
    }
}

/// Removes a memory, its vector and (by cascade) its versions, sources and
/// drift events. Whatever it had superseded is current again; that one's ID is returned.
pub(crate) fn remove_memory(tx: &Connection, id: i64, now: i64) -> Result<Option<i64>> {
    let restored: Option<i64> = tx
        .query_row(
            "SELECT id FROM memories WHERE superseded_by = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = restored {
        tx.execute(
            "UPDATE memories SET status = 'active', superseded_by = NULL, updated_at = ?1 WHERE id = ?2",
            params![now, old],
        )?;
    }
    // vec0 is a virtual table, so it isn't part of the foreign-key cascade.
    tx.execute("DELETE FROM memory_vectors WHERE memory_id = ?1", [id])?;
    tx.execute("DELETE FROM memories WHERE id = ?1", [id])?;
    Ok(restored)
}

/// A new interpretation for a memory, with the vector to match.
pub(crate) struct Rewrite<'a> {
    pub id: i64,
    pub interpretation: &'a str,
    pub embedding: &'a [u8],
    /// The memory_versions reason: drift or reinterpreted.
    pub reason: &'static str,
    pub drift_event: Option<i64>,
}

/// Replaces the interpretation and records the version and the new vector.
/// The schema refuses this for seared memories.
pub(crate) fn rewrite_interpretation(
    tx: &Connection,
    w: &Rewrite,
    persona_hash: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE memories SET interpretation = ?1, persona_hash = ?2, updated_at = ?3 WHERE id = ?4",
        params![w.interpretation, persona_hash, now, w.id],
    )?;
    tx.execute(
        "INSERT INTO memory_versions (memory_id, interpretation, level, persona_hash, reason, drift_event_id, created_at)
         SELECT id, interpretation, level, ?1, ?2, ?3, ?4 FROM memories WHERE id = ?5",
        params![persona_hash, w.reason, w.drift_event, now, w.id],
    )?;
    // vec0 rows are replaced, not updated in place.
    tx.execute("DELETE FROM memory_vectors WHERE memory_id = ?1", [w.id])?;
    tx.execute(
        "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
        params![w.id, w.embedding],
    )?;
    Ok(())
}

fn validate(draft: &MemoryDraft) -> Result<()> {
    let invalid = |msg: &str| Err(Error::Invalid(msg.to_owned()));
    if draft.interpretation.trim().is_empty() {
        return invalid("interpretation is empty");
    }
    if !draft.facts.is_empty() && draft.source_turns.is_empty() {
        return invalid("facts must cite the turns they come from");
    }
    match (draft.kind == MemoryType::Opinion, draft.strength) {
        (true, None) => return invalid("an opinion needs a strength"),
        (false, Some(_)) => return invalid("only opinions have a strength"),
        (true, Some(s)) if !(0.0..=1.0).contains(&s) => return invalid("strength must be 0–1"),
        _ => {}
    }
    if draft.valence.is_some_and(|v| !(-1.0..=1.0).contains(&v)) {
        return invalid("valence must be −1–1");
    }
    if (draft.kind == MemoryType::OpenThread) != draft.urgency.is_some() {
        return invalid("open threads, and only open threads, have an urgency");
    }
    Ok(())
}

pub(crate) fn ensure_open(conn: &Connection, session: i64) -> Result<()> {
    let ended: Option<Option<i64>> = conn
        .query_row(
            "SELECT ended_at FROM sessions WHERE id = ?1",
            [session],
            |r| r.get(0),
        )
        .optional()?;
    match ended {
        None => Err(Error::Invalid(format!("session {session} does not exist"))),
        Some(Some(_)) => Err(Error::SessionClosed(session)),
        Some(None) => Ok(()),
    }
}

/// `replaces` is left out of the duplicate check: a supersede is meant to be similar.
#[allow(clippy::too_many_arguments)]
fn insert(
    tx: &Transaction,
    actor: Actor,
    draft: &MemoryDraft,
    embedding: &[u8],
    persona_hash: &str,
    now: i64,
    replaces: Option<i64>,
    duplicate_similarity: f32,
) -> Result<i64> {
    let mut turns = draft.source_turns.clone();
    turns.sort_unstable();
    turns.dedup();
    for &turn in &turns {
        let found = tx
            .query_row("SELECT 1 FROM turns WHERE id = ?1", [turn], |_| Ok(()))
            .optional()?;
        if found.is_none() {
            return Err(Error::Invalid(format!("source turn {turn} does not exist")));
        }
    }

    // Compared against every active memory of the type, not the nearest few of
    // all vectors: superseded and other-type memories can crowd a copy out of
    // those. One type's active memories are few enough to read each vector.
    let nearest: Option<(i64, f64)> = tx
        .query_row(
            "SELECT m.id, 1 - vec_distance_cosine(v.embedding, ?1) AS similarity
             FROM memories m JOIN memory_vectors v ON v.memory_id = m.id
             WHERE m.status = 'active' AND m.type = ?2 AND m.id IS NOT ?3
             ORDER BY similarity DESC LIMIT 1",
            params![embedding, draft.kind, replaces],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((existing, similarity)) = nearest
        && similarity as f32 >= duplicate_similarity
    {
        return Err(Error::Duplicate {
            existing,
            similarity: similarity as f32,
        });
    }

    tx.execute(
        "INSERT INTO memories (type, level, facts, interpretation, occurred_at, persona_hash,
                               strength, urgency, valence, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            draft.kind,
            draft.level,
            Value::Object(draft.facts.clone()).to_string(),
            draft.interpretation,
            draft.occurred_at,
            persona_hash,
            draft.strength,
            draft.urgency,
            draft.valence,
            now,
            now
        ],
    )?;
    let id = tx.last_insert_rowid();
    for turn in turns {
        tx.execute(
            "INSERT INTO memory_sources (memory_id, turn_id) VALUES (?1, ?2)",
            [id, turn],
        )?;
    }
    tx.execute(
        "INSERT INTO memory_versions (memory_id, interpretation, level, persona_hash, reason, created_at)
         VALUES (?1, ?2, ?3, ?4, 'written', ?5)",
        params![id, draft.interpretation, draft.level, persona_hash, now],
    )?;
    tx.execute(
        "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
        params![id, embedding],
    )?;
    let summary = json!({ "type": draft.kind.as_str(), "level": draft.level.as_str() });
    log(
        tx,
        actor.as_str(),
        "write",
        "memory",
        Some(id),
        &summary,
        now,
    )?;
    Ok(id)
}
