mod common;

use clanka_core::deps::{Embedder, LlmRequest, LlmResponse, ToolCall};
use clanka_core::doctor::FactIssue;
use clanka_core::model::{Actor, Level, MemoryType, Section, Speaker, Status, Urgency};
use clanka_core::soul::Soul;
use clanka_core::{Companion, Error, MemoryDraft, Mood, PersonaStatus, Ranking, Result};
use common::{DOCTOR_TOKEN, clock, count, deps, fixture, open, open_full, raw, soul_text};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

fn draft(kind: MemoryType, level: Level, text: &str) -> MemoryDraft {
    MemoryDraft::new(kind, level, text)
}

fn mood(cause: &str) -> Mood {
    Mood {
        valence: 0.1,
        energy: 0.1,
        warmth: 0.1,
        cause: cause.into(),
    }
}

fn ids(c: &Companion, query: &str) -> Vec<i64> {
    c.recall(query, &Ranking::default())
        .unwrap()
        .into_iter()
        .map(|r| r.memory.id)
        .collect()
}

/// A model that re-interprets the given memories once, then stops.
fn reinterpreter(
    calls: Vec<(i64, &'static str)>,
    prompts: Arc<Mutex<Vec<String>>>,
) -> impl FnMut(&LlmRequest) -> LlmResponse + Send {
    move |r: &LlmRequest| {
        let tool_calls = if r.messages.len() == 1 {
            prompts.lock().unwrap().push(r.messages[0].content.clone());
            calls
                .iter()
                .map(|(id, text)| ToolCall {
                    id: format!("re{id}"),
                    name: "reinterpret".into(),
                    arguments: json!({ "memory_id": id, "interpretation": text }).to_string(),
                })
                .collect()
        } else {
            Vec::new()
        };
        LlmResponse {
            text: String::new(),
            tool_calls,
        }
    }
}

#[test]
fn rollback_restores_the_past_and_leaves_seared_alone() {
    let f = fixture();
    let time = clock();
    let mut c = open_full(&f, |_: &LlmRequest| LlmResponse::default(), time.clone());
    let s = c.open_session().unwrap();
    let said = c.record_turn(s, Speaker::User, "My sister Laura").unwrap();
    // IDs are predictable: the third memory written below is `raised`.
    let model = reinterpreter(
        vec![(3, "Sam hates the concert hall, like Sam's dad")],
        Arc::default(),
    );
    drop(c);
    let mut c = open_full(&f, model, time.clone());
    let mut sister = draft(MemoryType::UserFact, Level::Normal, "Laura is Sam's sister");
    sister.facts.insert("sister".into(), json!("Laura"));
    sister.source_turns = vec![said];
    let sister = c.write(Actor::Writer, &sister).unwrap();
    let mut thread = draft(
        MemoryType::OpenThread,
        Level::Normal,
        "Why did Laura leave?",
    );
    thread.urgency = Some(Urgency::Hot);
    let thread = c.write(Actor::Writer, &thread).unwrap();
    let mut liked = draft(
        MemoryType::Preference,
        Level::Normal,
        "Sam likes the concert hall",
    );
    liked.source_turns = vec![said];
    let raised = c.write(Actor::Writer, &liked).unwrap();
    assert_eq!(raised, 3);
    let to_sear = c
        .write(
            Actor::Writer,
            &draft(
                MemoryType::PersonaNote,
                Level::Normal,
                "Sam's dad scares me",
            ),
        )
        .unwrap();
    c.set_mood(Actor::Writer, &mood("before")).unwrap();
    c.write_section(Actor::Writer, Section::AboutUser, "Has a sister")
        .unwrap();

    let point = time.load(Ordering::SeqCst);
    time.fetch_add(1000, Ordering::SeqCst);
    // Everything after this point is the corruption.
    let reply = c
        .record_turn(s, Speaker::Companion, "Your cousin?")
        .unwrap();
    let new = c
        .write(
            Actor::Writer,
            &draft(MemoryType::Episode, Level::Normal, "The concert was chaos"),
        )
        .unwrap();
    let mut cousin = draft(MemoryType::UserFact, Level::Normal, "Laura is Sam's cousin");
    cousin.facts.insert("cousin".into(), json!("Laura"));
    cousin.source_turns = vec![said];
    let cousin = c.supersede(Actor::Writer, sister, &cousin).unwrap();
    c.set_level(Actor::Writer, raised, Level::Vivid).unwrap();
    c.set_frozen(true).unwrap();
    c.reinterpret(&[raised]).unwrap();
    c.set_frozen(false).unwrap();
    assert!(ids(&c, "dad").contains(&raised));
    let born_seared = c
        .write(
            Actor::Writer,
            &draft(MemoryType::Relationship, Level::Seared, "Dad, forever"),
        )
        .unwrap();
    c.set_level(Actor::Writer, to_sear, Level::Seared).unwrap();
    c.resolve_thread(Actor::Writer, thread).unwrap();
    c.set_mood(Actor::Writer, &mood("after")).unwrap();
    c.write_section(Actor::Writer, Section::AboutUser, "Has a cousin")
        .unwrap();
    c.record_pushback(Actor::Writer, reply, false, "cousin")
        .unwrap();
    c.write_turn_note(Actor::Writer, reply, "cousin?").unwrap();

    let actions: Vec<String> = c
        .writes_since(point)
        .unwrap()
        .into_iter()
        .map(|w| w.action)
        .collect();
    assert_eq!(
        actions,
        [
            "write",
            "write",
            "supersede",
            "set_level",
            "freeze",
            "reinterpret",
            "unfreeze",
            "write",
            "set_level",
            "resolve_thread",
            "set_mood",
            "write_section",
            "pushback",
            "write_turn_note"
        ]
    );
    assert!(matches!(c.rollback_to(point), Err(Error::NotFrozen)));
    let db = raw(c.db_path(), "k");
    let turns = count(&db, "SELECT count(*) FROM turns");

    c.set_frozen(true).unwrap();
    let report = c.rollback_to(point).unwrap();
    assert_eq!(report.removed, [cousin, new]);
    assert_eq!(report.restored, [raised]);
    assert_eq!(report.reopened, [thread]);
    assert_eq!(report.kept_seared, [to_sear, born_seared]);
    assert_eq!(report.state_rows, 4);

    let m = c.memory(sister).unwrap();
    assert_eq!((m.status, m.superseded_by), (Status::Active, None));
    let r = c.memory(raised).unwrap();
    assert_eq!(
        (r.level, r.interpretation.as_str()),
        (Level::Normal, "Sam likes the concert hall")
    );
    assert!(!ids(&c, "dad").contains(&raised), "its old vector is back");
    assert_eq!(
        c.memory(to_sear).unwrap().level,
        Level::Seared,
        "seared never changes"
    );
    assert!(c.memory(born_seared).is_ok());
    assert_eq!(c.mood().unwrap().unwrap().cause, "before");
    assert_eq!(
        c.section(Section::AboutUser).unwrap().as_deref(),
        Some("Has a sister")
    );
    assert_eq!(
        (
            c.pushback_pressure().unwrap().0,
            c.last_turn_note().unwrap()
        ),
        (0, None)
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM turns"),
        turns,
        "the transcript is truth"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM memories_fts WHERE memories_fts MATCH 'cousin OR chaos'"
        ),
        0
    );
    assert!(!ids(&c, "concert").contains(&new));
    let ctx = c.assemble("Hi", 10_000, &Ranking::default()).unwrap();
    assert_eq!(
        ctx.hot_threads.iter().map(|m| m.id).collect::<Vec<_>>(),
        [thread],
        "the thread is open again"
    );
    assert!(matches!(
        c.write(
            Actor::Writer,
            &draft(MemoryType::Episode, Level::Faint, "x dad")
        ),
        Err(Error::Frozen)
    ));
}

#[test]
fn check_facts_finds_facts_their_turns_dont_back() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let s = c.open_session().unwrap();
    let t1 = c
        .record_turn(s, Speaker::User, "My sister LAURA called")
        .unwrap();
    let t2 = c
        .record_turn(s, Speaker::User, "I moved to Lisbon")
        .unwrap();
    let mut good = draft(MemoryType::UserFact, Level::Normal, "Laura is Sam's sister");
    good.facts.insert("sister".into(), json!("Laura"));
    good.source_turns = vec![t1];
    c.write(Actor::Writer, &good).unwrap();
    let mut bad = draft(
        MemoryType::Preference,
        Level::Normal,
        "Sam moved to the coast",
    );
    bad.facts.insert("city".into(), json!("Porto"));
    bad.source_turns = vec![t2];
    let bad = c.write(Actor::Writer, &bad).unwrap();
    // Corruption that the write path would never allow: facts without sources.
    raw(c.db_path(), "k")
        .execute(
            "INSERT INTO memories (id, type, level, facts, interpretation, persona_hash, created_at, updated_at)
             VALUES (99, 'episode', 'normal', '{\"day\":\"Monday\"}', 'orphan', 'h', 0, 0)",
            [],
        )
        .unwrap();

    assert_eq!(
        c.check_facts().unwrap(),
        [
            FactIssue {
                memory: bad,
                fact: Some(("city".into(), "Porto".into()))
            },
            FactIssue {
                memory: 99,
                fact: None
            }
        ]
    );
}

#[test]
fn reinterpret_rereads_the_transcript_but_never_seared() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let s = c.open_session().unwrap();
    let t = c
        .record_turn(s, Speaker::User, "Laura took me to a concert")
        .unwrap();
    let cited = |c: &mut Companion, kind, level, text: &str| {
        let mut d = draft(kind, level, text);
        d.source_turns = vec![t];
        c.write(Actor::Writer, &d).unwrap()
    };
    let normal = cited(
        &mut c,
        MemoryType::Episode,
        Level::Normal,
        "Laura dragged Sam somewhere",
    );
    let seared = cited(
        &mut c,
        MemoryType::Relationship,
        Level::Seared,
        "Laura is everything to Sam",
    );
    let uncited = c
        .write(
            Actor::Writer,
            &draft(MemoryType::PersonaNote, Level::Normal, "I like dad jokes"),
        )
        .unwrap();
    drop(c);

    let prompts = Arc::default();
    let model = reinterpreter(
        vec![
            (normal, "Sam's dad took Sam to a concert"),
            (seared, "nothing"),
        ],
        Arc::clone(&prompts),
    );
    let mut c = open_full(&f, model, clock());
    assert!(matches!(c.reinterpret(&[normal]), Err(Error::NotFrozen)));
    c.set_frozen(true).unwrap();
    assert!(matches!(c.reinterpret(&[normal, seared]), Err(Error::Seared(id)) if id == seared));
    assert!(matches!(c.reinterpret(&[uncited]), Err(Error::Invalid(_))));
    assert!(
        prompts.lock().unwrap().is_empty(),
        "refused before calling the model"
    );

    let reflection = c.reinterpret(&[normal]).unwrap();
    assert_eq!(
        reflection.calls.iter().map(|o| o.ok).collect::<Vec<_>>(),
        [true, false]
    );
    assert!(
        prompts.lock().unwrap()[0].contains("What was said:\n(user) Laura took me to a concert")
    );
    assert_eq!(
        c.memory(normal).unwrap().interpretation,
        "Sam's dad took Sam to a concert"
    );
    assert_eq!(
        c.memory(seared).unwrap().interpretation,
        "Laura is everything to Sam"
    );
    assert!(ids(&c, "dad concert").contains(&normal), "re-embedded");
    let db = raw(c.db_path(), "k");
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT count(*) FROM memory_versions WHERE memory_id = {normal} AND reason = 'reinterpreted'"
            )
        ),
        1
    );
}

#[test]
fn update_persona_reinterprets_under_the_new_soul_and_unfreezes() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let s = c.open_session().unwrap();
    let t = c
        .record_turn(s, Speaker::User, "Laura and dad came to the concert")
        .unwrap();
    let mut ids = Vec::new();
    for (kind, level, text) in [
        (MemoryType::Episode, Level::Normal, "Laura came"),
        (MemoryType::Preference, Level::Faint, "dad came"),
        (MemoryType::Relationship, Level::Seared, "the concert"),
    ] {
        let mut d = draft(kind, level, text);
        d.source_turns = vec![t];
        ids.push(c.write(Actor::Writer, &d).unwrap());
    }
    drop(c);
    std::fs::write(&f.soul, soul_text(f.id, 0.5, &["Honesty over comfort"])).unwrap();
    let new_hash = Soul::load(&f.soul).unwrap().hash;

    let prompts = Arc::default();
    let model = reinterpreter(
        vec![(ids[0], "Laura came, finally"), (ids[1], "dad came, sadly")],
        Arc::clone(&prompts),
    );
    let mut c = open_full(&f, model, clock());
    assert!(matches!(c.persona_status(), PersonaStatus::Changed { .. }));
    c.update_persona().unwrap();

    assert!(
        !prompts.lock().unwrap()[0].contains(&format!("#{} [", ids[2])),
        "seared isn't offered"
    );
    for id in &ids[..2] {
        assert_eq!(c.memory(*id).unwrap().persona_hash, new_hash);
    }
    assert_ne!(
        c.memory(ids[2]).unwrap().persona_hash,
        new_hash,
        "seared keeps the soul it was felt under"
    );
    let db = raw(c.db_path(), "k");
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT count(*) FROM memory_versions WHERE reason = 'reinterpreted' AND persona_hash = '{new_hash}'"
            )
        ),
        2
    );
    assert!(!c.is_frozen().unwrap());
    assert_eq!(
        open(&f, "k").unwrap().persona_status(),
        &PersonaStatus::Unchanged
    );
}

#[test]
fn a_fork_archives_her_and_starts_anew_without_deleting() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    c.write(
        Actor::Writer,
        &draft(MemoryType::Episode, Level::Normal, "Laura"),
    )
    .unwrap();
    let old_db = c.db_path().to_owned();
    c.archive().unwrap();
    assert!(matches!(
        c.write(
            Actor::Writer,
            &draft(MemoryType::Episode, Level::Normal, "dad")
        ),
        Err(Error::Archived)
    ));
    c.set_frozen(true).unwrap();
    assert!(matches!(c.rollback_to(0), Err(Error::Archived)));
    drop(c);

    std::fs::write(
        &f.soul,
        soul_text(Uuid::now_v7(), 0.7, &["Honesty over comfort"]),
    )
    .unwrap();
    let mut fresh = open(&f, "k").unwrap();
    assert_eq!(fresh.persona_status(), &PersonaStatus::New);
    assert!(
        fresh
            .recall("Laura", &Ranking::default())
            .unwrap()
            .is_empty()
    );
    fresh
        .write(
            Actor::Writer,
            &draft(MemoryType::Episode, Level::Normal, "Laura"),
        )
        .unwrap();
    assert_eq!(
        count(&raw(&old_db, "k"), "SELECT count(*) FROM memories"),
        1,
        "the old one is still there"
    );
}

/// A different model with more dimensions, as a real model change has.
struct Wider;
impl Embedder for Wider {
    fn model_id(&self) -> &str {
        "model-c"
    }
    fn dimensions(&self) -> usize {
        5
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let inner = common::KeywordEmbedder("inner").embed(texts)?;
        Ok(inner
            .into_iter()
            .map(|mut v| {
                v.push(0.0);
                v
            })
            .collect())
    }
}

struct Broken;
impl Embedder for Broken {
    fn model_id(&self) -> &str {
        "model-b"
    }
    fn dimensions(&self) -> usize {
        4
    }
    fn embed(&self, _: &[&str]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Embedder("model file missing".into()))
    }
}

#[test]
fn reembed_rebuilds_every_vector_or_none() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let laura = c
        .write(
            Actor::Writer,
            &draft(MemoryType::Episode, Level::Normal, "Laura"),
        )
        .unwrap();
    c.write(
        Actor::Writer,
        &draft(MemoryType::Preference, Level::Normal, "dad"),
    )
    .unwrap();
    drop(c);
    let b = || Companion::open(f.dir.path(), &f.soul, deps("k", "model-b"));
    assert!(matches!(b(), Err(Error::EmbedderChanged { .. })));

    let mut broken = deps("k", "model-b");
    broken.embedder = Box::new(Broken);
    assert!(Companion::reembed(f.dir.path(), &f.soul, broken).is_err());
    assert_eq!(
        ids(&open(&f, "k").unwrap(), "Laura"),
        [laura],
        "the old vectors survive a failed re-embed"
    );

    let wider = || {
        let mut d = deps("k", "unused");
        d.embedder = Box::new(Wider);
        d
    };
    let c = Companion::reembed(f.dir.path(), &f.soul, wider()).unwrap();
    assert_eq!(ids(&c, "Laura"), [laura]);
    assert_eq!(
        count(
            &raw(c.db_path(), "k"),
            "SELECT count(*) FROM memory_vectors"
        ),
        2
    );
    drop(c);
    assert!(Companion::open(f.dir.path(), &f.soul, wider()).is_ok());
    assert!(matches!(open(&f, "k"), Err(Error::EmbedderChanged { .. })));
}

#[test]
fn the_doctor_token_is_exact_and_required() {
    let id = Uuid::now_v7();
    let soul = Soul::parse(&soul_text(id, 0.7, &["a"])).unwrap();
    assert!(soul.is_doctor_token(DOCTOR_TOKEN));
    assert!(soul.is_doctor_token(&format!("  {DOCTOR_TOKEN}\n")));
    for near in [
        &DOCTOR_TOKEN[1..],
        &format!("{DOCTOR_TOKEN}x"),
        "",
        &format!("please {DOCTOR_TOKEN}"),
    ] {
        assert!(!soul.is_doctor_token(near), "{near:?}");
    }
    let text = soul_text(id, 0.7, &["a"]);
    let short = text.replace(DOCTOR_TOKEN, "short");
    let missing = text.replace(&format!("doctor_token = \"{DOCTOR_TOKEN}\"\n"), "");
    for (bad, reason) in [(short, "at least 16"), (missing, "doctor_token")] {
        let err = Soul::parse(&bad).unwrap_err().to_string();
        assert!(err.contains(reason), "{err}");
    }
}
