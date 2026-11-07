use clanka::turn::{self, Begin};
use clanka_core::deps::{Clock, Embedder, KeySource, Llm, LlmRequest, LlmResponse, Rng};
use clanka_core::{Companion, Deps, Result};

struct Dummy;
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
impl Clock for Dummy {
    fn now_ms(&self) -> i64 {
        1
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

const TOKEN: &str = "0123456789abcdef0123";

#[test]
fn the_doctor_token_freezes_memory_and_is_never_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let soul = dir.path().join("soul.md");
    std::fs::write(
        &soul,
        format!(
            "+++\nid = \"{}\"\nname = \"Nova\"\ncontrariness = 0.5\npersistence = 0.5\n\
             time_sense = false\nrecall_window = 2\ndrift = 0.3\ntired_at = 0.7\nsleep_at = 0.9\n\
             pushback_window = 20\ndoctor_token = \"{TOKEN}\"\n+++\n# Nova\n",
            uuid::Uuid::now_v7()
        ),
    )
    .unwrap();
    let deps = Deps {
        llm: Box::new(Dummy),
        embedder: Box::new(Dummy),
        clock: Box::new(Dummy),
        rng: Box::new(Dummy),
        keys: Box::new(Dummy),
    };
    let mut c = Companion::open(dir.path(), &soul, deps).unwrap();

    // Surrounding whitespace doesn't hide the token.
    assert_eq!(
        turn::begin(&mut c, &format!(" {TOKEN}\n"), 4000).unwrap(),
        Begin::Doctor
    );
    assert!(c.is_frozen().unwrap());
    assert_eq!(turn::begin(&mut c, "hello?", 4000).unwrap(), Begin::Frozen);

    // Neither message reached the transcript: the first turn after unfreezing is turn 1.
    c.set_frozen(false).unwrap();
    let Begin::Open { pending, .. } = turn::begin(&mut c, "hi", 4000).unwrap() else {
        panic!("not open");
    };
    assert_eq!(pending.user_turn, 1);
}
