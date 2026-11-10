#![allow(dead_code)]

use clanka_core::deps::{Clock, Embedder, KeySource, Llm, LlmRequest, LlmResponse, Rng};
use clanka_core::{Companion, Deps, Result};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

pub struct Dummy;
impl Llm for Dummy {
    fn complete(&self, _: &LlmRequest) -> Result<LlmResponse> {
        Ok(LlmResponse::default())
    }
}
impl Embedder for Dummy {
    fn model_id(&self) -> &str {
        "dummy"
    }
    fn dimensions(&self) -> usize {
        2
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
}
/// Every reading is one millisecond later than the last.
struct Ticking(AtomicI64);
impl Clock for Ticking {
    fn now_ms(&self) -> i64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}
impl Rng for Dummy {
    fn next_seed(&self) -> u64 {
        1
    }
}
impl KeySource for Dummy {
    fn key(&self, _: uuid::Uuid) -> Result<String> {
        Ok("k".into())
    }
}

/// A model whose every reply comes from a closure, so tests script it and keep
/// what it was asked.
type Reply = Box<dyn FnMut(&LlmRequest) -> Result<LlmResponse> + Send>;
pub struct Scripted(Mutex<Reply>);
impl Scripted {
    pub fn new(reply: impl FnMut(&LlmRequest) -> Result<LlmResponse> + Send + 'static) -> Self {
        Self(Mutex::new(Box::new(reply)))
    }
}
impl Llm for Scripted {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse> {
        (self.0.lock().unwrap())(request)
    }
}

pub const TOKEN: &str = "0123456789abcdef0123";

/// A handle to the companion in `dir` with a model that says nothing.
pub fn open(dir: &Path) -> Companion {
    open_with(dir, Dummy)
}

/// A handle to the companion in `dir`, whose soul.md the first handle writes.
pub fn open_with(dir: &Path, llm: impl Llm + 'static) -> Companion {
    let soul = dir.join("soul.md");
    if !soul.exists() {
        std::fs::write(
            &soul,
            format!(
                "+++\nid = \"{}\"\nname = \"Nova\"\ncontrariness = 0.5\npersistence = 0.5\n\
                 time_sense = false\nrecall_window = 2\ndrift = 0.3\ntired_at = 0.7\n\
                 sleep_at = 0.9\npushback_window = 20\ndoctor_token = \"{TOKEN}\"\n+++\n# Nova\n",
                uuid::Uuid::now_v7()
            ),
        )
        .unwrap();
    }
    let deps = Deps {
        llm: Box::new(llm),
        embedder: Box::new(Dummy),
        clock: Box::new(Ticking(AtomicI64::new(1))),
        rng: Box::new(Dummy),
        keys: Box::new(Dummy),
    };
    Companion::open(dir, &soul, deps).unwrap()
}
