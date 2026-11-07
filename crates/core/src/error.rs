use crate::model::Level;
use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("wrong key, or {0} is not a companion database")]
    WrongKey(PathBuf),
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "database {path} has schema version {found}, newer than this build supports ({supported})"
    )]
    SchemaTooNew {
        path: PathBuf,
        found: i64,
        supported: i64,
    },
    #[error("{path} is in journal mode {mode}; WAL is required so several processes can share it")]
    NoWal { path: PathBuf, mode: String },
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid soul: {0}")]
    Soul(String),
    #[error("database belongs to companion {stored}, but the soul says {soul}")]
    CompanionMismatch {
        stored: uuid::Uuid,
        soul: uuid::Uuid,
    },
    #[error(
        "vectors were built with {stored}, but the embedder is {current}; run the doctor to re-embed"
    )]
    EmbedderChanged { stored: String, current: String },
    #[error("memory writes are frozen by the doctor")]
    Frozen,
    #[error("the doctor works only while writes are frozen; freeze first")]
    NotFrozen,
    #[error("this companion is archived and read-only")]
    Archived,
    #[error(
        "the persona changed; resolve it with `clanka soul continue`, `update` or `fork`, then \
         reopen the companion"
    )]
    PersonaUnresolved,
    #[error("session {0} has ended")]
    SessionClosed(i64),
    #[error("memory {0} not found")]
    NotFound(i64),
    #[error(
        "near-duplicate of memory {existing} (similarity {similarity:.2}); supersede it instead"
    )]
    Duplicate { existing: i64, similarity: f32 },
    #[error("the companion can only raise a level, not lower it from {current:?} to {requested:?}")]
    LevelDown { current: Level, requested: Level },
    #[error("memory {0} is seared and never changes")]
    Seared(i64),
    #[error("invalid write: {0}")]
    Invalid(String),
    #[error("key source: {0}")]
    Key(String),
    #[error("embedder: {0}")]
    Embedder(String),
    #[error("llm: {0}")]
    Llm(String),
}
