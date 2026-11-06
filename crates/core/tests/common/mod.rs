#![allow(dead_code)]

use clanka_core::deps::{Clock, Embedder, KeySource, Llm, LlmRequest, LlmResponse, Rng};
use clanka_core::{Companion, Deps, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

struct NoLlm;
impl Llm for NoLlm {
    fn complete(&self, _: &LlmRequest) -> Result<LlmResponse> {
        unreachable!("the storage layer must not call the model")
    }
}

const KEYWORDS: [&str; 3] = ["laura", "concert", "dad"];

/// One dimension per keyword plus a constant, so tests pick which texts are
/// similar: same keywords means cosine 1, different keywords means well below.
pub struct KeywordEmbedder(pub &'static str);
impl Embedder for KeywordEmbedder {
    fn model_id(&self) -> &str {
        self.0
    }
    fn dimensions(&self) -> usize {
        KEYWORDS.len() + 1
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let t = t.to_lowercase();
                let mut v: Vec<f32> = KEYWORDS
                    .iter()
                    .map(|k| if t.contains(k) { 1.0 } else { 0.0 })
                    .collect();
                v.push(0.1);
                v
            })
            .collect())
    }
}

/// Ticks one second per read, so every timestamp is distinct and ordered.
/// Tests jump it forward through the shared handle.
pub struct TestClock(pub Arc<AtomicI64>);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.fetch_add(1000, Ordering::SeqCst)
    }
}

pub fn clock() -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(1_700_000_000_000))
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

/// A model whose every reply comes from a closure, so tests script it.
type Reply = Box<dyn FnMut(&LlmRequest) -> LlmResponse + Send>;
pub struct FnLlm(Mutex<Reply>);
impl Llm for FnLlm {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        Ok((self.0.lock().unwrap())(request))
    }
}

pub fn open_with_llm(
    f: &Fixture,
    llm: impl FnMut(&LlmRequest) -> LlmResponse + Send + 'static,
) -> Companion {
    open_full(f, llm, clock())
}

pub fn open_full(
    f: &Fixture,
    llm: impl FnMut(&LlmRequest) -> LlmResponse + Send + 'static,
    clock: Arc<AtomicI64>,
) -> Companion {
    let mut deps = deps("k", "model-a");
    deps.llm = Box::new(FnLlm(Mutex::new(Box::new(llm))));
    deps.clock = Box::new(TestClock(clock));
    Companion::open(f.dir.path(), &f.soul, deps).unwrap()
}

pub fn deps(key: &'static str, model: &'static str) -> Deps {
    Deps {
        llm: Box::new(NoLlm),
        embedder: Box::new(KeywordEmbedder(model)),
        clock: Box::new(TestClock(clock())),
        rng: Box::new(FixedRng),
        keys: Box::new(Key(key)),
    }
}

pub const DOCTOR_TOKEN: &str = "5f1c0e2a9b7d4c3e8a6f";

pub fn soul_text(id: Uuid, tired_at: f64, values: &[&str]) -> String {
    let values: String = values.iter().map(|v| format!("- {v}\n")).collect();
    format!(
        "+++\nid = \"{id}\"\nname = \"Ani\"\ncontrariness = 0.6\npersistence = 0.7\n\
         time_sense = true\nrecall_window = 2\ndrift = 0.4\ntired_at = {tired_at}\n\
         sleep_at = 0.9\npushback_window = 20\ndoctor_token = \"{DOCTOR_TOKEN}\"\n+++\n# Ani\n\n## Core values\n\n{values}\n\
         ## Voice\n\n- Not a value, just style\n"
    )
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub soul: PathBuf,
    pub id: Uuid,
}

pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let id = Uuid::now_v7();
    let soul = dir.path().join("soul.md");
    std::fs::write(&soul, soul_text(id, 0.7, &["Honesty over comfort"])).unwrap();
    Fixture { dir, soul, id }
}

pub fn open(f: &Fixture, key: &'static str) -> Result<Companion> {
    Companion::open(f.dir.path(), &f.soul, deps(key, "model-a"))
}

/// A raw connection, as another process would open the file.
pub fn raw(path: &Path, key: &str) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "key", key).unwrap();
    conn
}

pub fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}
