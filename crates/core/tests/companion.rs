use clanka_core::deps::{Clock, Embedder, KeySource, Llm, LlmRequest, LlmResponse, Rng};
use clanka_core::soul::Soul;
use clanka_core::{Companion, Deps, Error, PersonaStatus, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use uuid::Uuid;

struct NoLlm;
impl Llm for NoLlm {
    fn complete(&self, _: &LlmRequest) -> Result<LlmResponse> {
        unreachable!("opening a companion must not call the model")
    }
}

struct FakeEmbedder(&'static str);
impl Embedder for FakeEmbedder {
    fn model_id(&self) -> &str {
        self.0
    }
    fn dimensions(&self) -> usize {
        4
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0; 4]).collect())
    }
}

struct FixedClock;
impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        1_700_000_000_000
    }
}

struct FixedRng;
impl Rng for FixedRng {
    fn next_seed(&self) -> u64 {
        42
    }
}

struct Key(&'static str);
impl KeySource for Key {
    fn key(&self, _: Uuid) -> Result<String> {
        Ok(self.0.to_owned())
    }
}

fn deps(key: &'static str, model: &'static str) -> Deps {
    Deps {
        llm: Box::new(NoLlm),
        embedder: Box::new(FakeEmbedder(model)),
        clock: Box::new(FixedClock),
        rng: Box::new(FixedRng),
        keys: Box::new(Key(key)),
    }
}

fn soul_text(id: Uuid, tired_at: f64, values: &[&str]) -> String {
    let values: String = values.iter().map(|v| format!("- {v}\n")).collect();
    format!(
        "+++\nid = \"{id}\"\nname = \"Ani\"\ncontrariness = 0.6\npersistence = 0.7\n\
         time_sense = true\nrecall_window = 2\ndrift = 0.4\ntired_at = {tired_at}\n\
         sleep_at = 0.9\npushback_window = 20\n+++\n# Ani\n\n## Core values\n\n{values}\n\
         ## Voice\n\n- Not a value, just style\n"
    )
}

struct Fixture {
    dir: tempfile::TempDir,
    soul: PathBuf,
    id: Uuid,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let id = Uuid::now_v7();
    let soul = dir.path().join("soul.md");
    std::fs::write(&soul, soul_text(id, 0.7, &["Honesty over comfort"])).unwrap();
    Fixture { dir, soul, id }
}

fn open(f: &Fixture, key: &'static str) -> Result<Companion> {
    Companion::open(f.dir.path(), &f.soul, deps(key, "model-a"))
}

/// A raw connection, as another process would open the file.
fn raw(path: &Path, key: &str) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "key", key).unwrap();
    conn
}

#[test]
fn persona_change_is_detected_until_continued() {
    let f = fixture();
    assert_eq!(open(&f, "k").unwrap().persona_status(), &PersonaStatus::New);
    assert_eq!(
        open(&f, "k").unwrap().persona_status(),
        &PersonaStatus::Unchanged
    );

    let accepted = Soul::load(&f.soul).unwrap().hash;
    std::fs::write(&f.soul, soul_text(f.id, 0.5, &["Honesty over comfort"])).unwrap();
    let mut c = open(&f, "k").unwrap();
    assert_eq!(
        c.persona_status(),
        &PersonaStatus::Changed {
            accepted_hash: accepted
        }
    );

    // Still blocked on the next open until someone chooses.
    assert!(matches!(
        open(&f, "k").unwrap().persona_status(),
        PersonaStatus::Changed { .. }
    ));
    c.continue_with_persona().unwrap();
    assert_eq!(
        open(&f, "k").unwrap().persona_status(),
        &PersonaStatus::Unchanged
    );
}

#[test]
fn database_is_encrypted_and_rejects_the_wrong_key() {
    let f = fixture();
    let path = open(&f, "right").unwrap().db_path().to_owned();
    let header = std::fs::read(&path).unwrap();
    assert!(!header.starts_with(b"SQLite format 3"), "file is plaintext");
    assert!(matches!(open(&f, "wrong"), Err(Error::WrongKey(_))));
}

#[test]
fn freeze_from_one_handle_is_seen_by_another() {
    let f = fixture();
    let a = open(&f, "k").unwrap();
    let b = open(&f, "k").unwrap();
    assert!(!b.is_frozen().unwrap());
    a.set_frozen(true).unwrap();
    assert!(b.is_frozen().unwrap());
    b.set_frozen(false).unwrap();
    assert!(!a.is_frozen().unwrap());
}

#[test]
fn concurrent_first_opens_migrate_once() {
    let f = fixture();
    let (dir, soul) = (f.dir.path().to_owned(), f.soul.clone());
    let threads = 8;
    let start = std::sync::Arc::new(std::sync::Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let (dir, soul, start) = (dir.clone(), soul.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                Companion::open(&dir, &soul, deps("k", "model-a")).map(|_| ())
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap().unwrap();
    }
}

#[test]
fn changing_the_embedder_requires_a_re_embed() {
    let f = fixture();
    open(&f, "k").unwrap();
    let err = Companion::open(f.dir.path(), &f.soul, deps("k", "model-b"))
        .err()
        .unwrap();
    assert!(
        matches!(err, Error::EmbedderChanged { ref stored, ref current }
        if stored == "model-a" && current == "model-b")
    );
}

#[test]
fn storage_rules_hold_for_any_process() {
    let f = fixture();
    let path = open(&f, "k").unwrap().db_path().to_owned();
    let db = raw(&path, "k");
    db.execute_batch(
        "INSERT INTO sessions (id, started_at) VALUES (1, 0);
         INSERT INTO turns (id, session_id, role, content, created_at)
           VALUES (1, 1, 'user', 'My sister Laura moved to Lisbon', 0);
         INSERT INTO memories (id, type, level, facts, interpretation, persona_hash, created_at, updated_at)
           VALUES (1, 'user_fact', 'normal', '{\"sister\":\"Laura\"}', 'He misses Laura more than he says', 'h', 0, 0),
                  (2, 'episode', 'seared', '{}', 'The night he told me about his dad', 'h', 0, 0);
         INSERT INTO memory_vectors (memory_id, embedding) VALUES (1, '[0.1, 0.2, 0.3, 0.4]');",
    )
    .unwrap();

    let refused = |sql: &str, reason: &str| {
        let err = db.execute_batch(sql).unwrap_err().to_string();
        assert!(err.contains(reason), "{sql}: {err}");
    };
    refused(
        "UPDATE memories SET facts = '{\"sister\":\"Lara\"}' WHERE id = 1",
        "facts are immutable",
    );
    refused(
        "UPDATE memories SET interpretation = 'blurred' WHERE id = 2",
        "seared memories never change",
    );
    refused(
        "UPDATE memories SET level = 'faint' WHERE id = 2",
        "seared memories never change",
    );
    refused(
        "UPDATE turns SET content = 'edited' WHERE id = 1",
        "turns are immutable",
    );

    // Interpretations of non-seared memories can drift, and search follows them.
    db.execute(
        "UPDATE memories SET interpretation = 'He keeps going quiet about Laura' WHERE id = 1",
        [],
    )
    .unwrap();
    let hit = |q: &str| -> Vec<i64> {
        let mut s = db
            .prepare("SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1")
            .unwrap();
        s.query_map([q], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(hit("quiet"), vec![1]);
    assert!(hit("misses").is_empty());

    let nearest: i64 = db
        .query_row(
            "SELECT memory_id FROM memory_vectors WHERE embedding MATCH '[0.1, 0.2, 0.3, 0.4]' AND k = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nearest, 1);
}

#[test]
fn soul_errors_refuse_to_load() {
    let id = Uuid::now_v7();
    let too_many = ["a", "b", "c", "d", "e", "f"];
    for (text, reason) in [
        (soul_text(id, 0.9, &["a"]), "tired_at"),
        (soul_text(id, 0.7, &too_many), "6 core values"),
        (
            soul_text(
                Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap(),
                0.7,
                &["a"],
            ),
            "UUIDv7",
        ),
        (
            soul_text(id, 0.7, &["a"]).replace("drift = 0.4", "drift = 1.5"),
            "drift",
        ),
        (
            soul_text(id, 0.7, &["a"]).replace("name = \"Ani\"", "name = \"Ani\"\nmood = 1"),
            "mood",
        ),
        ("# no frontmatter".to_owned(), "frontmatter"),
    ] {
        let err = Soul::parse(&text).unwrap_err().to_string();
        assert!(err.contains(reason), "expected {reason:?} in {err:?}");
    }

    let soul = Soul::parse(&soul_text(
        id,
        0.7,
        &["Honesty over comfort", "Never cruel"],
    ))
    .unwrap();
    assert_eq!(soul.core_values, ["Honesty over comfort", "Never cruel"]);
}
