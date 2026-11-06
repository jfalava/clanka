use crate::soul::Soul;
use crate::{Deps, Error, Result, db};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// One companion: her soul.md and her database file `<data_dir>/<id>.db`.
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
}

/// How the soul.md on disk relates to the one her memories were written under.
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

        ensure_vectors(&conn, deps.embedder.model_id(), deps.embedder.dimensions())?;

        let persona = match meta(&conn, "persona_hash")? {
            None => {
                set_meta(&conn, "persona_hash", &soul.hash)?;
                log(
                    &conn,
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
            Some(accepted_hash) => PersonaStatus::Changed { accepted_hash },
        };

        Ok(Self {
            conn,
            soul,
            db_path,
            deps,
            persona,
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

    pub fn deps(&self) -> &Deps {
        &self.deps
    }

    pub fn persona_status(&self) -> &PersonaStatus {
        &self.persona
    }

    /// "Continue" after a persona change: keep old memories as she felt them
    /// and write new ones under the new persona.
    pub fn continue_with_persona(&mut self) -> Result<()> {
        set_meta(&self.conn, "persona_hash", &self.soul.hash)?;
        let hash = json!({ "hash": self.soul.hash });
        log(
            &self.conn,
            "cli",
            "persona_continued",
            "persona",
            None,
            &hash,
            self.deps.clock.now_ms(),
        )?;
        self.persona = PersonaStatus::Unchanged;
        Ok(())
    }

    /// Whether the doctor has frozen writes. Read from the database each time,
    /// so a freeze from another process applies at once.
    pub fn is_frozen(&self) -> Result<bool> {
        Ok(meta(&self.conn, "frozen")?.as_deref() == Some("1"))
    }

    pub fn set_frozen(&self, frozen: bool) -> Result<()> {
        set_meta(&self.conn, "frozen", if frozen { "1" } else { "0" })?;
        let action = if frozen { "freeze" } else { "unfreeze" };
        log(
            &self.conn,
            "doctor",
            action,
            "companion",
            None,
            &json!({}),
            self.deps.clock.now_ms(),
        )
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

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
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
    if meta(conn, "frozen")?.as_deref() == Some("1") {
        return Err(Error::Frozen);
    }
    if meta(conn, "persona_hash")?.as_deref() != Some(soul_hash) {
        return Err(Error::PersonaUnresolved);
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
