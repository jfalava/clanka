mod common;

use clanka_core::deps::{LlmRequest, LlmResponse, ToolCall};
use clanka_core::dream::{exaggerate, hint};
use clanka_core::model::{Actor, Level, MemoryType, SessionEnd, Speaker, Urgency};
use clanka_core::soul::Soul;
use clanka_core::{Companion, Error, MemoryDraft, Ranking};
use common::{clock, count, fixture, open, open_full, open_with_llm, raw};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

const DAY: i64 = 24 * 3_600_000;

/// The test RNG's seed 42 through SplitMix64 (0.741565), times the fixture's
/// drift of 0.4 pushed toward the low end it leans to (0.08), times a faint
/// memory's cap of 1: computed outside the code under test.
const DREAM_FAINT_DRIFT: f64 = 0.059_325;

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: format!("{name}-{args}"),
        name: name.into(),
        arguments: args.to_string(),
    }
}

/// A model that makes `calls` on the first round of the first wake and keeps
/// every prompt it was given; later rounds make none.
fn waker(
    calls: Vec<ToolCall>,
    prompts: Arc<Mutex<Vec<String>>>,
) -> impl FnMut(&LlmRequest) -> LlmResponse + Send {
    let mut calls = Some(calls);
    move |r: &LlmRequest| {
        prompts.lock().unwrap().push(r.messages[0].content.clone());
        let tool_calls = if r.messages.len() == 1 {
            calls.take().unwrap_or_default()
        } else {
            Vec::new()
        };
        LlmResponse {
            text: String::new(),
            tool_calls,
        }
    }
}

fn mood(cause: &str) -> ToolCall {
    call(
        "settle_mood",
        json!({ "valence": -0.6, "energy": 0.2, "warmth": 0.4, "cause": cause }),
    )
}

/// One waking evening, slept on by the doctor (no model): the user says
/// `said`, the companion replies.
fn awake_evening(c: &mut Companion, said: &str) -> i64 {
    let s = c.open_session().unwrap();
    let turn = c.record_turn(s, Speaker::User, said).unwrap();
    c.record_turn(s, Speaker::Companion, "Mm.").unwrap();
    c.end_session(s, SessionEnd::Doctor).unwrap();
    turn
}

/// The survival check is code, not a prompt: a fact stays only when an echo of
/// this dream showed it and the user had written it while the companion was
/// awake, out of earshot included. Whatever the dream made up, whatever was
/// said awake but never echoed, and a word that turns up everywhere all fade.
/// A name from the companion's own life proves nothing, and a name only the
/// dream knew can't ride along in the interpretation either.
#[test]
fn an_echo_sears_only_what_was_written_awake() {
    let f = fixture();
    // Greta is in Nova's own life.
    let soul = std::fs::read_to_string(&f.soul).unwrap();
    std::fs::write(
        &f.soul,
        format!("{soul}\nNova grew up with her aunt Greta.\n"),
    )
    .unwrap();
    let clk = clock();
    let mut c = open_full(&f, |_: &LlmRequest| LlmResponse::default(), clk.clone());
    let whisper = awake_evening(
        &mut c,
        "Sam leans in: ||the judge sentenced Laura to hang at Dawnfort, Greta knew|| and Nova is busy.",
    );
    let faint = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(MemoryType::Preference, Level::Faint, "Laura likes jazz"),
        )
        .unwrap();
    let normal = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(MemoryType::PersonaNote, Level::Normal, "My dad is strict"),
        )
        .unwrap();
    clk.fetch_add(DAY, Ordering::SeqCst);

    let dream = c.start_dream().unwrap();
    c.record_turn(dream, Speaker::User, "Sam is at the door, in red.")
        .unwrap();
    c.record_turn(dream, Speaker::Companion, "I let Sam in.")
        .unwrap();
    c.record_echo(
        dream,
        "Years ago the judge says Laura will hang at Dawnfort, below Morrow Hill. Greta nods.",
    )
    .unwrap();
    c.record_turn(dream, Speaker::Companion, "No. No, not her.")
        .unwrap();
    c.mark_recalled(dream, &[faint, normal]).unwrap();
    drop(c);

    let sear_as = |interpretation: &str, facts: Value| {
        call(
            "sear",
            json!({ "type": "user_fact", "facts": facts, "interpretation": interpretation,
                    "to_confirm": "Is Laura really condemned?" }),
        )
    };
    let sear = |facts: Value| sear_as("Laura was condemned, and nobody told me", facts);
    let calls = vec![
        // Only what the dream made up: nothing stays.
        sear(json!({ "hill": "Morrow Hill" })),
        // Echoed and written awake, but Greta is Nova's own: it proves nothing.
        sear(json!({ "who": "Greta" })),
        // The facts hold, but the interpretation names a hill only the dream knew.
        sear_as(
            "Laura hangs at Dawnfort, below Morrow Hill",
            json!({ "place": "Dawnfort" }),
        ),
        sear(
            json!({ "place": "Dawnfort", "hill": "Morrow Hill", "who": "Sam", "word": "the", "aunt": "Greta" }),
        ),
        // One thing stays from a dream; a second is refused.
        sear(json!({ "who": "Laura" })),
        mood("I woke cold, and I don't know why"),
        call(
            "keep_fragment",
            json!({ "interpretation": "a red coat in a doorway" }),
        ),
        call(
            "keep_fragment",
            json!({ "interpretation": "the pale face of Laura" }),
        ),
        call(
            "keep_fragment",
            json!({ "interpretation": "the smell of Laura\u{2019}s kitchen" }),
        ),
        call(
            "keep_fragment",
            json!({ "interpretation": "a concert somewhere far off" }),
        ),
        call(
            "drift",
            json!({ "memory_id": faint, "interpretation": "Laura likes old music" }),
        ),
        call(
            "drift",
            json!({ "memory_id": normal, "interpretation": "Dad was kind" }),
        ),
    ];
    let prompts = Arc::default();
    let mut c = open_full(&f, waker(calls, Arc::clone(&prompts)), clk.clone());
    let report = c.wake(dream).unwrap();

    let seared = c.memory(report.seared.unwrap()).unwrap();
    assert_eq!(
        (
            seared.level,
            seared.dreamt,
            Value::Object(seared.facts.clone())
        ),
        (
            Level::Seared,
            true,
            json!({ "place": "Dawnfort", "aunt": "Greta" })
        )
    );
    assert_eq!(seared.source_turns, [whisper], "it cites the waking turn");
    let db = raw(c.db_path(), "k");
    assert_eq!(
        count(&db, "SELECT count(*) FROM memories WHERE level = 'seared'"),
        1
    );
    let thread = c.memory(report.thread.unwrap()).unwrap();
    assert_eq!(
        (thread.kind, thread.urgency, thread.interpretation.as_str()),
        (
            MemoryType::OpenThread,
            Some(Urgency::Hot),
            "Is Laura really condemned?"
        )
    );

    // The named fragments were refused, however the possessive was written;
    // the others are faint and factless.
    assert_eq!(report.fragments.len(), 2);
    for id in &report.fragments {
        let m = c.memory(*id).unwrap();
        assert!(m.dreamt && m.level == Level::Faint && m.facts.is_empty());
    }

    // Drift follows the dial pushed the way it leans: 0.4 goes down, so the
    // normal memory's drift falls under the least offered and isn't offered.
    assert_eq!(report.drifted.len(), 1);
    let (id, magnitude) = report.drifted[0];
    assert_eq!(id, faint);
    assert!((magnitude - DREAM_FAINT_DRIFT).abs() < 1e-5, "{magnitude}");
    assert_eq!(c.memory(normal).unwrap().interpretation, "My dad is strict");

    // The wake pass read the dream with echoes marked, and nothing of the
    // waking evening: the whisper isn't the companion's to see.
    let prompt = &prompts.lock().unwrap()[0];
    assert!(prompt.contains("(echo) Years ago the judge"), "{prompt}");
    assert!(
        prompt.contains("(around you) Sam is at the door"),
        "{prompt}"
    );
    assert!(!prompt.contains("Nova is busy"), "{prompt}");

    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT count(*) FROM sessions WHERE id = {dream} AND ended_by = 'sleep' AND dream = 1"
            )
        ),
        1
    );
    assert_eq!(c.dreaming().unwrap(), None);
    assert_eq!(
        c.mood().unwrap().unwrap().cause,
        "I woke cold, and I don't know why"
    );

    // Awake, the fragments never come up; what the echo seared does, with no
    // age even for a soul with a sense of time, and saying where it came from.
    clk.fetch_add(30 * DAY, Ordering::SeqCst);
    let ctx = c
        .assemble("Laura at the concert", 4000, &Ranking::default())
        .unwrap();
    let recalled: Vec<i64> = ctx.memories.iter().map(|r| r.memory.id).collect();
    assert!(recalled.contains(&seared.id), "{recalled:?}");
    for fragment in &report.fragments {
        assert!(!recalled.contains(fragment), "{recalled:?}");
    }
    let text = ctx.render();
    assert!(
        text.contains(
            "- Laura was condemned, and nobody told me (you learned this in a dream, and it is \
             burned into you) [aunt: Greta, place: Dawnfort]"
        ),
        "{text}"
    );
    assert!(
        text.contains("- (4 weeks ago) Laura likes old music"),
        "{text}"
    );

    // In the next dream they come back.
    c.start_dream().unwrap();
    let dreamt: Vec<i64> = c
        .recall("a concert", &Ranking::default())
        .unwrap()
        .iter()
        .map(|r| r.memory.id)
        .collect();
    assert!(
        report.fragments.iter().any(|id| dreamt.contains(id)),
        "{dreamt:?}"
    );
}

/// Waking without a mood isn't waking: the dream stays open, nothing ends,
/// and a retry finishes it.
#[test]
fn a_wake_without_a_mood_leaves_the_dream_open() {
    let f = fixture();
    let clk = clock();
    let mut c = open_full(&f, |_: &LlmRequest| LlmResponse::default(), clk.clone());
    let dream = c.start_dream().unwrap();
    c.record_turn(dream, Speaker::User, "You're on a train.")
        .unwrap();
    c.record_turn(dream, Speaker::Companion, "Where to?")
        .unwrap();
    assert!(matches!(c.wake(dream), Err(Error::Llm(_))));
    assert_eq!(c.dreaming().unwrap(), Some(dream));
    drop(c);

    let mut c = open_full(&f, waker(vec![mood("A long ride")], Arc::default()), clk);
    let report = c.wake(dream).unwrap();
    assert_eq!(report.seared, None);
    assert_eq!(c.dreaming().unwrap(), None);
}

/// A dream starts only after sleep; it belongs to the handle that dreams it,
/// and any other handle finds it lost and drops it with nothing written. Its
/// exchanges are never reflected on: that would call the model, and the
/// fixture's model refuses every call.
#[test]
fn a_dream_starts_after_sleep_and_a_lost_one_is_dropped() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let s = c.open_session().unwrap();
    c.record_turn(s, Speaker::User, "hi").unwrap();
    assert!(matches!(c.start_dream(), Err(Error::Awake)));
    c.end_session(s, SessionEnd::Doctor).unwrap();

    // An open session nothing was said in becomes the dream.
    let empty = c.open_session().unwrap();
    let dream = c.start_dream().unwrap();
    assert_eq!(dream, empty);
    assert!(matches!(
        c.sleep(dream),
        Err(Error::Invalid(why)) if why.contains("dream")
    ));
    let said = c
        .record_turn(dream, Speaker::User, "You're by the sea.")
        .unwrap();
    let replied = c
        .record_turn(dream, Speaker::Companion, "It's so loud.")
        .unwrap();
    let reflection = c.reflect(said, replied).unwrap();
    assert_eq!(reflection.rounds, 0);
    assert_eq!(c.drop_lost_dream().unwrap(), None, "it is this handle's");

    let mut other = open(&f, "k").unwrap();
    assert!(matches!(other.wake(s), Err(Error::SessionClosed(_))));
    assert_eq!(other.drop_lost_dream().unwrap(), Some(dream));
    assert_eq!(other.dreaming().unwrap(), None);
    let db = raw(other.db_path(), "k");
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT count(*) FROM sessions WHERE id = {dream}
                 AND ended_at IS NOT NULL AND ended_by IS NULL"
            )
        ),
        1
    );
    assert_eq!(count(&db, "SELECT count(*) FROM memories"), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM reflections"), 0);
    // An echo outside a dream is refused: there is nothing for it to show.
    let awake = other.open_session().unwrap();
    assert!(other.record_echo(awake, "Years ago").is_err());
}

/// What was written out of earshot is the transcript's, never memory's: the
/// writer reads the exchange without it, and a fact quoted from it is left out.
#[test]
fn the_writer_never_hears_what_was_said_out_of_earshot() {
    let f = fixture();
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = requests.clone();
    let mut c = open_with_llm(&f, move |r: &LlmRequest| {
        seen.lock().unwrap().push(r.messages[0].content.clone());
        let tool_calls = if r.messages.len() == 1 {
            vec![call(
                "remember",
                json!({ "type": "user_fact", "level": "faint",
                        "interpretation": "Sam is planning something", "facts": { "place": "Dawnfort" } }),
            )]
        } else {
            Vec::new()
        };
        LlmResponse {
            text: String::new(),
            tool_calls,
        }
    });
    let s = c.open_session().unwrap();
    let said = c
        .record_turn(s, Speaker::User, "Back soon. ||Off to Dawnfort.||")
        .unwrap();
    let replied = c.record_turn(s, Speaker::Companion, "Okay!").unwrap();
    let reflection = c.reflect(said, replied).unwrap();
    assert!(
        reflection.calls[0].result.contains("left out"),
        "{reflection:?}"
    );

    let prompt = &requests.lock().unwrap()[0];
    assert!(prompt.contains("Back soon."), "{prompt}");
    assert!(!prompt.contains("Dawnfort"), "{prompt}");
    let kept = c.active_memories().unwrap();
    assert!(kept[0].facts.is_empty(), "{kept:?}");
}

/// In a dream each dial is pushed toward the end it leans to and reaches the
/// model as a sentence; a dial at the middle leans neither way.
#[test]
fn the_dream_hint_turns_each_dial_up_the_way_it_leans() {
    assert!((exaggerate(0.7) - 0.94).abs() < 1e-9);
    assert!((exaggerate(0.3) - 0.06).abs() < 1e-9);
    assert_eq!(exaggerate(0.5), 0.5);

    let f = fixture(); // contrariness 0.6, persistence 0.7
    let soul = Soul::load(&f.soul).unwrap();
    let leaning = hint(&soul);
    assert!(leaning.contains("push back on everything"), "{leaning}");
    assert!(leaning.contains("can't let anything go"), "{leaning}");
    assert!(
        !leaning.contains("dream"),
        "realizing it is what wakes: {leaning}"
    );

    let text = std::fs::read_to_string(&f.soul)
        .unwrap()
        .replace("contrariness = 0.6", "contrariness = 0.2")
        .replace("persistence = 0.7", "persistence = 0.5");
    let level = hint(&Soul::parse(&text).unwrap());
    assert!(level.contains("nothing in you pushes back"), "{level}");
    assert!(!level.contains("let anything go"), "{level}");
    assert!(!level.contains("slides off you"), "{level}");
}
