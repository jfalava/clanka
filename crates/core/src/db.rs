//! Opening a companion database: SQLCipher key, WAL for several processes,
//! sqlite-vec, and schema migrations tracked in `PRAGMA user_version`.

use crate::{Error, Result};
use rusqlite::{Connection, OpenFlags, ffi};
use std::path::Path;
use std::sync::Once;
use std::time::Duration;

const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_init.sql"),
    include_str!("migrations/0002_memory_valence.sql"),
    include_str!("migrations/0003_pushbacks.sql"),
    include_str!("migrations/0004_reflections.sql"),
    include_str!("migrations/0005_reflection_refunds.sql"),
];

/// How long a write waits for another process holding the lock.
/// Page cache per connection, in KiB.
const CACHE_KIB: i64 = 64 * 1024;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn open(path: &Path, key: &str) -> Result<Connection> {
    register_sqlite_vec();
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    conn.pragma_update(None, "key", key)?;
    // Before the first read: other processes may hold the lock already.
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // The first read is where SQLCipher notices a wrong key.
    if let Err(e) = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    }) {
        return Err(match e.sqlite_error_code() {
            Some(rusqlite::ErrorCode::NotADatabase) => Error::WrongKey(path.to_owned()),
            _ => e.into(),
        });
    }
    let mode = enable_wal(&conn)?;
    if mode != "wal" {
        return Err(Error::NoWal {
            path: path.to_owned(),
            mode,
        });
    }
    conn.pragma_update(None, "foreign_keys", true)?;
    // Vectors are read on every recall. SQLCipher decrypts each page it reads,
    // so they must stay in the page cache: 64 MiB holds ~40k 384-dim vectors.
    conn.pragma_update(None, "cache_size", -CACHE_KIB)?;
    migrate(&conn, path)?;
    Ok(conn)
}

/// Switching a new file to WAL needs an exclusive lock, and SQLite returns BUSY
/// at once instead of calling the busy handler when two first opens race. WAL
/// is persistent, so only the very first opens of a file ever retry here.
fn enable_wal(conn: &Connection) -> Result<String> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0)) {
            Err(e)
                if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return Ok(result?),
        }
    }
}

fn migrate(conn: &Connection, path: &Path) -> Result<()> {
    let supported = MIGRATIONS.len() as i64;
    let found: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if found > supported {
        return Err(Error::SchemaTooNew {
            path: path.to_owned(),
            found,
            supported,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let target = index as i64 + 1;
        // IMMEDIATE takes the write lock first; re-reading the version inside it
        // means a second process opening a fresh file skips what the first applied.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if current < target {
                conn.execute_batch(sql)?;
                conn.pragma_update(None, "user_version", target)?;
            }
            conn.execute_batch("COMMIT")
        })();
        if let Err(e) = result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e.into());
        }
    }
    Ok(())
}

fn register_sqlite_vec() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // sqlite-vec exposes the standard extension entry point; auto-extension
        // registration makes it available on every connection opened afterwards.
        #[allow(clippy::missing_transmute_annotations)]
        ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}
