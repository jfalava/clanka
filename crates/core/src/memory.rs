//! The write path: sessions, the immutable transcript and memories.
//!
//! Every write checks the doctor's freeze and the accepted persona inside its
//! own IMMEDIATE transaction, so a freeze from another process applies to the
//! very next write. Embeddings are computed before the transaction opens, so
//! other processes never wait on the model.

use crate::companion::{ensure_writable, log};
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
    /// Opinions only: how strongly she holds the stance, 0–1.
    pub strength: Option<f64>,
    /// Open threads only.
    pub urgency: Option<Urgency>,
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
    pub source_turns: Vec<i64>,
    pub created_at: i64,
    pub updated_at: i64,
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
        let id = insert(&tx, actor, draft, &embedding, &self.soul.hash, now, None)?;
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
        if actor == Actor::Writer && level < current {
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
            Actor::Writer => "escalated",
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

    pub fn memory(&self, id: i64) -> Result<Memory> {
        let (mut memory, facts) = self
            .conn
            .query_row(
                "SELECT id, type, level, facts, interpretation, status, superseded_by, occurred_at,
                        persona_hash, strength, urgency, created_at, updated_at
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
    fn embed(&self, text: &str) -> Result<Vec<u8>> {
        let dimensions = self.deps.embedder.dimensions();
        let vector = self
            .deps
            .embedder
            .embed(&[text])?
            .pop()
            .filter(|v| v.len() == dimensions)
            .ok_or_else(|| {
                Error::Embedder(format!("expected one vector of {dimensions} dimensions"))
            })?;
        Ok(vector.iter().flat_map(|x| x.to_le_bytes()).collect())
    }
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
    if (draft.kind == MemoryType::OpenThread) != draft.urgency.is_some() {
        return invalid("open threads, and only open threads, have an urgency");
    }
    Ok(())
}

fn ensure_open(conn: &Connection, session: i64) -> Result<()> {
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
fn insert(
    tx: &Transaction,
    actor: Actor,
    draft: &MemoryDraft,
    embedding: &[u8],
    persona_hash: &str,
    now: i64,
    replaces: Option<i64>,
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

    // A full scan, not a KNN query: it has to be restricted to one type and
    // active memories, and a companion's memories number in the thousands.
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
        && similarity as f32 >= DUPLICATE_SIMILARITY
    {
        return Err(Error::Duplicate {
            existing,
            similarity: similarity as f32,
        });
    }

    tx.execute(
        "INSERT INTO memories (type, level, facts, interpretation, occurred_at, persona_hash,
                               strength, urgency, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            draft.kind,
            draft.level,
            Value::Object(draft.facts.clone()).to_string(),
            draft.interpretation,
            draft.occurred_at,
            persona_hash,
            draft.strength,
            draft.urgency,
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
