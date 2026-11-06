mod common;

use clanka_core::deps::{LlmRequest, LlmResponse, ToolCall};
use clanka_core::model::{Actor, Level, MemoryType, SessionEnd, Speaker};
use clanka_core::{Companion, Error, MemoryDraft, Mood, Ranking};
use common::{clock, count, fixture, open_full, raw, soul_text};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

const HOUR: i64 = 3_600_000;

/// Drift draws for the test RNG's seed 42 (0.7416 through SplitMix64) times the
/// soul's drift of 0.4 and each level's cap, computed outside the code under test.
const FAINT: f64 = 0.296_626;
const NORMAL: f64 = 0.177_976;
const VIVID: f64 = 0.074_156;

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: format!("{name}-{}", args),
        name: name.into(),
        arguments: args.to_string(),
    }
}

/// A model that makes `calls` on the first round of each sleep and then stops.
fn sleeper(
    mut nights: Vec<Vec<ToolCall>>,
    prompts: Arc<Mutex<Vec<String>>>,
) -> impl FnMut(&LlmRequest) -> LlmResponse + Send {
    nights.reverse();
    move |r: &LlmRequest| {
        let tool_calls = if r.messages.len() == 1 {
            prompts.lock().unwrap().push(r.messages[0].content.clone());
            nights.pop().unwrap_or_default()
        } else {
            Vec::new()
        };
        LlmResponse {
            text: String::new(),
            tool_calls,
        }
    }
}

fn episode(text: &str) -> ToolCall {
    call(
        "write_episode",
        json!({ "interpretation": text, "valence": 0.6 }),
    )
}

fn write(c: &mut Companion, kind: MemoryType, level: Level, text: &str) -> i64 {
    c.write(Actor::Writer, &MemoryDraft::new(kind, level, text))
        .unwrap()
}

#[test]
fn sleep_writes_the_night_and_drifts_only_what_came_up() {
    let f = fixture();
    let prompts = Arc::default();
    let mut c = open_full(&f, |_: &LlmRequest| LlmResponse::default(), clock());
    let s = c.open_session().unwrap();
    let said = c
        .record_turn(s, Speaker::User, "My sister Laura called")
        .unwrap();
    c.record_turn(s, Speaker::Companion, "How is she?").unwrap();

    let faint = write(
        &mut c,
        MemoryType::Preference,
        Level::Faint,
        "Laura likes jazz",
    );
    let mut sister = MemoryDraft::new(MemoryType::UserFact, Level::Normal, "Laura is his sister");
    sister.facts.insert("sister".into(), json!("Laura"));
    sister.source_turns = vec![said];
    let normal = c.write(Actor::Writer, &sister).unwrap();
    let vivid = write(
        &mut c,
        MemoryType::Relationship,
        Level::Vivid,
        "Laura scares me a little",
    );
    let seared = write(
        &mut c,
        MemoryType::Episode,
        Level::Seared,
        "The day Laura left",
    );
    let unrecalled = write(
        &mut c,
        MemoryType::PersonaNote,
        Level::Normal,
        "His dad is strict",
    );
    c.mark_recalled(s, &[faint, normal, vivid, seared]).unwrap();
    drop(c);

    let night = vec![
        episode("The concert talk made me laugh"),
        call(
            "settle_mood",
            json!({ "valence": 0.5, "energy": -0.6, "warmth": 0.7, "cause": "A good night" }),
        ),
        call(
            "drift",
            json!({ "memory_id": faint, "interpretation": "Laura likes some old music" }),
        ),
        call(
            "drift",
            json!({ "memory_id": normal, "interpretation": "His sister took him to a concert once" }),
        ),
        call(
            "drift",
            json!({ "memory_id": vivid, "interpretation": "Laura scares me, a bit" }),
        ),
        call(
            "drift",
            json!({ "memory_id": seared, "interpretation": "Nothing happened" }),
        ),
        call(
            "drift",
            json!({ "memory_id": unrecalled, "interpretation": "Dad is fine" }),
        ),
    ];
    let mut c = open_full(&f, sleeper(vec![night], Arc::clone(&prompts)), clock());
    let report = c.sleep(s).unwrap();

    let drifted: Vec<i64> = report.drifted.iter().map(|d| d.0).collect();
    assert_eq!(drifted, [faint, normal, vivid]);
    for (&(_, got), want) in report.drifted.iter().zip([FAINT, NORMAL, VIVID]) {
        assert!(
            (got - want).abs() < 1e-5,
            "magnitude {got}, expected {want}"
        );
    }
    let prompt = &prompts.lock().unwrap()[0];
    assert!(
        prompt.contains(&format!("#{faint} [faint, drift 0.30] Laura likes jazz")),
        "{prompt}"
    );
    assert!(prompt.contains("(him) My sister Laura called"), "{prompt}");
    for absent in [seared, unrecalled] {
        assert!(
            !prompt.contains(&format!("#{absent} [")),
            "{absent} offered:\n{prompt}"
        );
    }

    // Facts never drift; the vector follows the new interpretation.
    let m = c.memory(normal).unwrap();
    assert_eq!(
        (m.interpretation.as_str(), m.facts["sister"].as_str()),
        ("His sister took him to a concert once", Some("Laura"))
    );
    let found: Vec<i64> = c
        .recall("concert", &Ranking::default())
        .unwrap()
        .iter()
        .map(|r| r.memory.id)
        .collect();
    assert!(found.contains(&normal), "{found:?}");
    assert_eq!(
        c.memory(seared).unwrap().interpretation,
        "The day Laura left"
    );
    assert_eq!(
        c.memory(unrecalled).unwrap().interpretation,
        "His dad is strict"
    );
    let db = raw(c.db_path(), "k");
    assert_eq!(
        count(&db, &format!("SELECT count(*) FROM memory_versions v JOIN drift_events d ON d.id = v.drift_event_id
                             WHERE v.reason = 'drift' AND d.seed = '42' AND d.session_id = {s} AND d.memory_id = {normal}")),
        1
    );

    assert_eq!(c.memory(report.episode).unwrap().kind, MemoryType::Episode);
    assert_eq!(c.mood().unwrap().unwrap().cause, "A good night");
    assert_ne!(c.open_session().unwrap(), s, "the night's session ended");
}

#[test]
fn no_episode_means_no_sleep_and_a_retry_never_drifts_twice() {
    let f = fixture();
    let time = clock();
    let mut c = open_full(&f, |_: &LlmRequest| LlmResponse::default(), time.clone());
    let s = c.open_session().unwrap();
    let id = write(
        &mut c,
        MemoryType::Preference,
        Level::Faint,
        "Laura likes jazz",
    );
    c.mark_recalled(s, &[id]).unwrap();
    drop(c);

    let drift = || {
        call(
            "drift",
            json!({ "memory_id": id, "interpretation": "Laura likes some music" }),
        )
    };
    let mut c = open_full(
        &f,
        sleeper(vec![vec![drift()]], Arc::default()),
        time.clone(),
    );
    assert!(matches!(c.sleep(s), Err(Error::Llm(_))));
    let record = c.record_turn(s, Speaker::User, "still awake?");
    assert!(record.is_ok(), "the session stays open: {record:?}");
    drop(c);

    let mut c = open_full(
        &f,
        sleeper(
            vec![vec![episode("concert night"), drift()]],
            Arc::default(),
        ),
        time,
    );
    let report = c.sleep(s).unwrap();
    assert!(report.drifted.is_empty(), "already drifted this session");
    assert_eq!(
        count(&raw(c.db_path(), "k"), "SELECT count(*) FROM drift_events"),
        1
    );
}

#[test]
fn threads_left_open_escalate_with_persistence_up_to_vivid() {
    let f = fixture(); // persistence 0.7: one level per 2 sessions open
    let combos = [
        "laura",
        "concert",
        "dad",
        "laura concert",
        "laura dad",
        "concert dad",
        "laura concert dad",
    ];
    let nights = combos
        .iter()
        .map(|t| vec![episode(&format!("night of {t}"))])
        .collect();
    let mut c = open_full(&f, sleeper(nights, Arc::default()), clock());

    let mut s = c.open_session().unwrap();
    let thread = |c: &mut Companion, text: &str| {
        let mut d = MemoryDraft::new(MemoryType::OpenThread, Level::Faint, text);
        d.urgency = Some(clanka_core::model::Urgency::Simmering);
        c.write(Actor::Writer, &d).unwrap()
    };
    let open = thread(&mut c, "Why did he skip dinner with his dad?");
    let closed = thread(&mut c, "Did Laura call him at midnight?");
    c.resolve_thread(Actor::Writer, closed).unwrap();

    let mut levels = Vec::new();
    for night in 0..combos.len() {
        if night > 0 {
            s = c.open_session().unwrap();
        }
        c.sleep(s).unwrap();
        levels.push(c.memory(open).unwrap().level);
    }
    use Level::*;
    assert_eq!(levels, [Faint, Faint, Normal, Normal, Vivid, Vivid, Vivid]);
    assert_eq!(
        c.memory(closed).unwrap().level,
        Faint,
        "a resolved thread stays where it was"
    );
}

fn parting(f: &common::Fixture, time: &Arc<AtomicI64>, valence: f64, warmth: f64) -> Companion {
    let mut c = open_full(f, |_: &LlmRequest| LlmResponse::default(), time.clone());
    let s = c.open_session().unwrap();
    let mood = Mood {
        valence,
        energy: 0.0,
        warmth,
        cause: "Last night".into(),
    };
    c.set_mood(Actor::Writer, &mood).unwrap();
    c.end_session(s, SessionEnd::Sleep).unwrap();
    c
}

fn warmth(c: &Companion) -> (f64, Option<f64>) {
    let ctx = c.assemble("Hi", 10_000, &Ranking::default()).unwrap();
    (ctx.mood.unwrap().warmth, ctx.time_away.map(|t| t.hours))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn time_away_warms_or_cools_her_and_fades_as_you_talk() {
    let f = fixture(); // time_sense = true
    let time = clock();
    let mut c = parting(&f, &time, 0.5, 0.2);
    assert_eq!(warmth(&c), (0.2, None), "no gap yet");

    time.fetch_add(48 * HOUR, Ordering::SeqCst);
    let (w, hours) = warmth(&c);
    assert!(
        close(w, 0.2 + 0.3 * 48.0 / 72.0) && close(hours.unwrap(), 48.0),
        "{w} {hours:?}"
    );

    let s = c.open_session().unwrap();
    for _ in 0..2 {
        c.record_turn(s, Speaker::User, "hey").unwrap();
    }
    assert!(
        close(warmth(&c).0, 0.2 + 0.1),
        "half faded after 2 of 4 messages"
    );
    for _ in 0..2 {
        c.record_turn(s, Speaker::User, "hey").unwrap();
    }
    assert_eq!(warmth(&c), (0.2, None), "gone after 4");
}

#[test]
fn a_bad_parting_cools_her_but_never_past_the_floor() {
    let f = fixture();
    let time = clock();
    let c = parting(&f, &time, -0.5, -0.2);
    time.fetch_add(100 * HOUR, Ordering::SeqCst);
    assert!(close(warmth(&c).0, -0.3), "{:?}", warmth(&c));

    let g = fixture();
    std::fs::write(
        &g.soul,
        soul_text(g.id, 0.7, &["Honesty"]).replace("time_sense = true", "time_sense = false"),
    )
    .unwrap();
    let time = clock();
    let c = parting(&g, &time, 0.5, 0.2);
    time.fetch_add(100 * HOUR, Ordering::SeqCst);
    assert_eq!(
        warmth(&c),
        (0.2, None),
        "a soul without time sense doesn't notice"
    );
}
