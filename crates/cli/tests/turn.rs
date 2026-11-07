use clanka::turn::{self, Begin};
use clanka_core::deps::{Clock, Embedder, KeySource, Llm, LlmRequest, LlmResponse, Rng};
use clanka_core::model::{Actor, Level, MemoryType, Section};
use clanka_core::{Companion, Deps, MemoryDraft, Result};
use std::sync::atomic::{AtomicI64, Ordering};

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

const TOKEN: &str = "0123456789abcdef0123";

fn open(dir: &std::path::Path) -> Companion {
    let soul = dir.join("soul.md");
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
        clock: Box::new(Ticking(AtomicI64::new(1))),
        rng: Box::new(Dummy),
        keys: Box::new(Dummy),
    };
    Companion::open(dir, &soul, deps).unwrap()
}

fn begin(c: &mut Companion, message: &str, first: bool) -> (String, turn::Pending) {
    match turn::begin(c, message, 4000, first).unwrap() {
        Begin::Open { context, pending } => (context, pending),
        other => panic!("not open: {other:?}"),
    }
}

#[test]
fn the_doctor_token_freezes_memory_and_is_never_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = open(dir.path());

    // Surrounding whitespace doesn't hide the token.
    assert_eq!(
        turn::begin(&mut c, &format!(" {TOKEN}\n"), 4000, true).unwrap(),
        Begin::Doctor
    );
    assert!(c.is_frozen().unwrap());
    assert_eq!(
        turn::begin(&mut c, "hello?", 4000, true).unwrap(),
        Begin::Frozen
    );

    // Neither message reached the transcript: the first turn after unfreezing is turn 1.
    c.set_frozen(false).unwrap();
    let (_, pending) = begin(&mut c, "hi", true);
    assert_eq!(pending.user_turn, 1);
}

/// A host that replays what it was given gets the whole context once, then
/// only what the model hasn't seen: a changed section, a new note, a memory not
/// yet recalled this session. A turn with nothing new is empty.
#[test]
fn later_turns_carry_only_what_is_new() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = open(dir.path());
    c.write_section(Actor::Cli, Section::AboutUser, "Likes tea.")
        .unwrap();
    // The dummy embedder puts every text in one spot, so a second memory of the
    // same type would be refused as a near-duplicate.
    let draft = |kind, text: &str| MemoryDraft::new(kind, Level::Normal, text);
    c.write(
        Actor::Cli,
        &draft(MemoryType::UserFact, "The user drinks tea"),
    )
    .unwrap();

    let (first, pending) = begin(&mut c, "hi", true);
    assert!(first.contains("Likes tea."), "{first}");
    assert!(first.contains("The user drinks tea"), "{first}");
    let (user, _) = turn::end(&mut c, pending, "hello").unwrap();

    // Nothing changed since: nothing to say.
    let (second, pending) = begin(&mut c, "still here", false);
    assert_eq!(second, "");
    turn::end(&mut c, pending, "yes").unwrap();

    // The writer left a note and a new memory and rewrote a section.
    c.write_turn_note(Actor::Writer, user, "They seemed tired.")
        .unwrap();
    c.write(
        Actor::Writer,
        &draft(MemoryType::Preference, "The user has a cat"),
    )
    .unwrap();
    c.write_section(Actor::Writer, Section::Relationship, "Easy.")
        .unwrap();
    let (third, pending) = begin(&mut c, "anything?", false);
    assert!(third.contains("They seemed tired."), "{third}");
    assert!(third.contains("The user has a cat"), "{third}");
    assert!(third.contains("Easy."), "{third}");
    assert!(!third.contains("Likes tea."), "{third}");
    assert!(!third.contains("The user drinks tea"), "{third}");
    turn::end(&mut c, pending, "no").unwrap();

    // A new host conversation against the same open session starts over.
    let (again, _) = begin(&mut c, "hi again", true);
    assert!(again.contains("Likes tea."), "{again}");
    assert!(again.contains("The user drinks tea"), "{again}");
}
