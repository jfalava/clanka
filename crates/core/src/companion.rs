use crate::deps::Embedder;
use crate::memory::DUPLICATE_SIMILARITY;
use crate::recall::Ranking;
use crate::soul::Soul;
use crate::{Deps, Error, Result, db};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// One companion: its soul.md and its database file `<data_dir>/<id>.db`.
///
/// Several processes may hold a handle to the same companion at once, so any
/// state they must agree on (the doctor's freeze, the accepted persona) lives in
/// the database and is read on each call, never cached here.
pub struct Companion {
    pub(crate) conn: Connection,
    pub(crate) soul: Soul,
    db_path: PathBuf,
    pub(crate) deps: Deps,
    persona: PersonaStatus,
    ranking: Ranking,
    pub(crate) duplicate_similarity: f32,
    /// The dream this handle started. A dream belongs to the handle that
    /// dreams it: another one that takes a turn finds it lost and drops it.
    pub(crate) dream: Option<i64>,
}

/// How the soul.md on disk relates to the one its memories were written under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonaStatus {
    /// First open: this persona is now the accepted one.
    New,
    Unchanged,
    /// The file changed. The front end must block chat until you choose to
    /// continue, update or fork.
    Changed {
        accepted_hash: String,
    },
}

impl Companion {
    pub fn open(data_dir: &Path, soul_path: &Path, deps: Deps) -> Result<Self> {
        Self::open_inner(data_dir, soul_path, deps, false)
    }

    /// The doctor's re-embed: opens the companion even though its vectors came
    /// from another embedding model, and rebuilds every vector with this one in
    /// a single transaction, so a failure leaves the old vectors in place.
    /// `progress` hears how many memories are embedded, of how many.
    pub fn reembed(
        data_dir: &Path,
        soul_path: &Path,
        deps: Deps,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Self> {
        let mut companion = Self::open_inner(data_dir, soul_path, deps, true)?;
        companion.rebuild_vectors(progress)?;
        Ok(companion)
    }

    fn open_inner(data_dir: &Path, soul_path: &Path, deps: Deps, reembed: bool) -> Result<Self> {
        let soul = Soul::load(soul_path)?;
        let id = soul.traits.id;
        let db_path = data_dir.join(format!("{id}.db"));
        let key = deps.keys.key(id)?;
        let conn = db::open(&db_path, &key)?;
        let now = deps.clock.now_ms();

        match meta(&conn, "companion_id")? {
            None => set_meta(&conn, "companion_id", &id.to_string())?,
            Some(stored) if stored == id.to_string() => {}
            Some(stored) => {
                let stored = stored.parse().unwrap_or(Uuid::nil());
                return Err(Error::CompanionMismatch { stored, soul: id });
            }
        }

        if !reembed {
            ensure_vectors(&conn, deps.embedder.model_id(), deps.embedder.dimensions())?;
        }

        // One transaction: an accepted hash is never stored without its log line.
        let tx = conn.unchecked_transaction()?;
        let persona = match meta(&tx, "persona_hash")? {
            None => {
                set_meta(&tx, "persona_hash", &soul.hash)?;
                log(
                    &tx,
                    "cli",
                    "persona_accepted",
                    "persona",
                    None,
                    &json!({ "hash": soul.hash }),
                    now,
                )?;
                PersonaStatus::New
            }
            Some(hash) if hash == soul.hash => PersonaStatus::Unchanged,
            // The same file under the old whole-file hash: nothing changed, so
            // move the database to the body hash instead of asking.
            Some(hash) if hash == soul.legacy_hash => {
                set_meta(&tx, "persona_hash", &soul.hash)?;
                log(
                    &tx,
                    "cli",
                    "persona_rehashed",
                    "persona",
                    None,
                    &json!({ "hash": soul.hash }),
                    now,
                )?;
                PersonaStatus::Unchanged
            }
            Some(accepted_hash) => PersonaStatus::Changed { accepted_hash },
        };
        tx.commit()?;

        Ok(Self {
            conn,
            soul,
            db_path,
            deps,
            persona,
            ranking: Ranking::default(),
            duplicate_similarity: DUPLICATE_SIMILARITY,
            dream: None,
        })
    }

    pub fn id(&self) -> Uuid {
        self.soul.traits.id
    }

    pub fn soul(&self) -> &Soul {
        &self.soul
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// How this handle ranks recall on its own turns: the front end's setting,
    /// the defaults unless [`Companion::set_ranking`] changed it.
    pub fn ranking(&self) -> &Ranking {
        &self.ranking
    }

    /// Refuses a ranking recall can't work with (see [`Ranking::check`]).
    pub fn set_ranking(&mut self, ranking: Ranking) -> Result<()> {
        ranking.check()?;
        self.ranking = ranking;
        Ok(())
    }

    /// The cosine similarity from which a new memory counts as a copy of one
    /// of the same type; [`DUPLICATE_SIMILARITY`] unless set. It depends on the
    /// embedding model: another model spreads similar texts differently.
    /// Above 0 and at most 1: anything else would refuse every write or none.
    pub fn set_duplicate_similarity(&mut self, similarity: f32) -> Result<()> {
        if !(similarity > 0.0 && similarity <= 1.0) {
            return Err(Error::Invalid(format!(
                "duplicate_similarity is {similarity}; it must be above 0 and at most 1"
            )));
        }
        self.duplicate_similarity = similarity;
        Ok(())
    }

    /// Writes a copy of the companion's whole database, as it is at this
    /// moment, to a new file under the same key. Safe while other processes
    /// write: the copy is one consistent snapshot. Refuses to overwrite a file.
    ///
    /// The companion isn't opened: a copy needs no embedding model, so one
    /// whose vectors came from another model, or whose persona changed, can
    /// still be moved.
    pub fn export(data_dir: &Path, soul: &Soul, key: &str, to: &Path) -> Result<()> {
        if to.exists() {
            return Err(Error::Invalid(format!("{} already exists", to.display())));
        }
        let target = to
            .to_str()
            .ok_or_else(|| Error::Invalid(format!("{} is not valid UTF-8", to.display())))?;
        let conn = db::open(&data_dir.join(format!("{}.db", soul.traits.id)), key)?;
        conn.execute("VACUUM INTO ?1", [target])?;
        Ok(())
    }

    pub fn deps(&self) -> &Deps {
        &self.deps
    }

    pub fn persona_status(&self) -> &PersonaStatus {
        &self.persona
    }

    /// "Continue" after a persona change: keep old memories as they were felt
    /// and write new ones under the new persona.
    pub fn continue_with_persona(&mut self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        set_meta(&tx, "persona_hash", &self.soul.hash)?;
        let hash = json!({ "hash": self.soul.hash });
        log(
            &tx,
            "cli",
            "persona_continued",
            "persona",
            None,
            &hash,
            self.deps.clock.now_ms(),
        )?;
        tx.commit()?;
        self.persona = PersonaStatus::Unchanged;
        Ok(())
    }

    /// Whether the doctor has frozen writes. Read from the database each time,
    /// so a freeze from another process applies at once.
    pub fn is_frozen(&self) -> Result<bool> {
        Ok(meta(&self.conn, "frozen")?.as_deref() == Some("1"))
    }

    pub fn set_frozen(&self, frozen: bool) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        set_meta(&tx, "frozen", if frozen { "1" } else { "0" })?;
        let action = if frozen { "freeze" } else { "unfreeze" };
        log(
            &tx,
            "doctor",
            action,
            "companion",
            None,
            &json!({}),
            self.deps.clock.now_ms(),
        )?;
        Ok(tx.commit()?)
    }
}

impl Companion {
    fn rebuild_vectors(&mut self, progress: &mut dyn FnMut(usize, usize)) -> Result<()> {
        // Embedding is slow, so it happens before the write lock is taken.
        let memories = interpretations(&self.conn)?;
        let embedder = &self.deps.embedder;
        let vectors = embed_all(embedder.as_ref(), &memories, progress)?;
        let mut ready: HashMap<i64, (String, Vec<u8>)> = memories
            .into_iter()
            .zip(vectors)
            .map(|((id, text), vector)| (id, (text, vector)))
            .collect();
        let (model, dimensions) = (embedder.model_id().to_owned(), embedder.dimensions());
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Another process may have written or reworded a memory meanwhile:
        // whatever isn't in the first pass is embedded now, under the lock.
        let current = interpretations(&tx)?;
        let late: Vec<(i64, String)> = current
            .iter()
            .filter(|(id, text)| ready.get(id).is_none_or(|(embedded, _)| embedded != text))
            .cloned()
            .collect();
        for ((id, text), vector) in
            late.iter()
                .cloned()
                .zip(embed_all(embedder.as_ref(), &late, &mut |_, _| {})?)
        {
            ready.insert(id, (text, vector));
        }
        tx.execute_batch("DROP TABLE IF EXISTS memory_vectors")?;
        tx.execute("DELETE FROM meta WHERE key = 'embedder_model'", [])?;
        ensure_vectors(&tx, &model, dimensions)?;
        for (id, _) in &current {
            tx.execute(
                "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
                params![id, ready[id].1],
            )?;
        }
        let summary = json!({ "model": model, "memories": current.len() });
        log(&tx, "doctor", "reembed", "companion", None, &summary, now)?;
        tx.commit()?;
        Ok(())
    }
}

fn interpretations(conn: &Connection) -> Result<Vec<(i64, String)>> {
    Ok(conn
        .prepare("SELECT id, interpretation FROM memories ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// One sqlite-vec blob per memory, in order.
fn embed_all(
    embedder: &dyn Embedder,
    memories: &[(i64, String)],
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Vec<Vec<u8>>> {
    let mut vectors = Vec::with_capacity(memories.len());
    for chunk in memories.chunks(64) {
        let texts: Vec<&str> = chunk.iter().map(|(_, t)| t.as_str()).collect();
        let embedded = embedder.embed(&texts)?;
        if embedded.len() != chunk.len() {
            return Err(Error::Embedder(format!(
                "expected {} vectors, got {}",
                chunk.len(),
                embedded.len()
            )));
        }
        for vector in &embedded {
            vectors.push(blob(vector, embedder.dimensions())?);
        }
        progress(vectors.len(), memories.len());
    }
    Ok(vectors)
}

/// A vector as sqlite-vec's little-endian f32 blob. Refuses one of the wrong
/// size, and one that is all zeros or not finite: its cosine similarity is
/// undefined, and a single such row makes every recall fail.
pub(crate) fn blob(vector: &[f32], dimensions: usize) -> Result<Vec<u8>> {
    if vector.len() != dimensions {
        return Err(Error::Embedder(format!(
            "expected a vector of {dimensions} dimensions, got {}",
            vector.len()
        )));
    }
    let norm: f32 = vector.iter().map(|x| x * x).sum();
    if !norm.is_finite() || norm == 0.0 {
        return Err(Error::Embedder(
            "the model returned a vector that is all zeros or not a number".into(),
        ));
    }
    Ok(vector.iter().flat_map(|x| x.to_le_bytes()).collect())
}

/// Checked inside every transaction that writes a vector: the doctor may have
/// re-embedded from another process after this handle embedded its text, and
/// a vector from the old model would sit among the new ones as noise.
pub(crate) fn ensure_embedder(conn: &Connection, model: &str) -> Result<()> {
    match meta(conn, "embedder_model")? {
        Some(stored) if stored != model => Err(Error::EmbedderChanged {
            stored,
            current: model.to_owned(),
        }),
        _ => Ok(()),
    }
}

/// Vectors are tied to the model that made them. A different model means the
/// doctor has to re-embed; searching old vectors with new queries is meaningless.
fn ensure_vectors(conn: &Connection, model: &str, dimensions: usize) -> Result<()> {
    match meta(conn, "embedder_model")? {
        Some(stored) if stored != model => {
            return Err(Error::EmbedderChanged {
                stored,
                current: model.to_owned(),
            });
        }
        Some(_) => {}
        None => {
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(\
                   memory_id INTEGER PRIMARY KEY, embedding float[{dimensions}] distance_metric=cosine)"
            ))?;
            set_meta(conn, "embedder_model", model)?;
        }
    }
    Ok(())
}

pub(crate) fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

pub(crate) fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Every write checks this inside its own transaction: the doctor's freeze and
/// the persona choice may come from another process at any moment.
pub(crate) fn ensure_writable(conn: &Connection, soul_hash: &str) -> Result<()> {
    if meta(conn, "archived")?.as_deref() == Some("1") {
        return Err(Error::Archived);
    }
    if meta(conn, "frozen")?.as_deref() == Some("1") {
        return Err(Error::Frozen);
    }
    if meta(conn, "persona_hash")?.as_deref() != Some(soul_hash) {
        return Err(Error::PersonaUnresolved);
    }
    Ok(())
}

/// The doctor's gate: it writes only while everything else is frozen.
pub(crate) fn ensure_doctor(conn: &Connection) -> Result<()> {
    if meta(conn, "archived")?.as_deref() == Some("1") {
        return Err(Error::Archived);
    }
    if meta(conn, "frozen")?.as_deref() != Some("1") {
        return Err(Error::NotFrozen);
    }
    Ok(())
}

pub(crate) fn log(
    conn: &Connection,
    actor: &str,
    action: &str,
    target: &str,
    target_id: Option<i64>,
    payload: &Value,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO write_log (actor, action, target, target_id, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![actor, action, target, target_id, payload.to_string(), now],
    )?;
    Ok(())
}
