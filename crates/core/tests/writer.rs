mod common;

use clanka_core::deps::{LlmRequest, LlmResponse, Role, ToolCall};
use clanka_core::model::{Actor, Level, MemoryType, Section, Speaker, Status};
use clanka_core::writer::MAX_ROUNDS;
use clanka_core::{Companion, Error, MemoryDraft, Ranking};
use common::{count, fixture, open, open_with_llm, raw};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type Requests = Arc<Mutex<Vec<LlmRequest>>>;

/// A model that makes the given tool calls, one list per round, then stops.
fn script(
    rounds: Vec<Vec<(&'static str, Value)>>,
) -> (impl FnMut(&LlmRequest) -> LlmResponse + Send, Requests) {
    let requests: Requests = Arc::default();
    let seen = requests.clone();
    let model = move |request: &LlmRequest| {
        let mut seen = seen.lock().unwrap();
        seen.push(request.clone());
        let round = seen.len() - 1;
        let tool_calls =
            rounds
                .get(round)
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, (name, args))| ToolCall {
                    id: format!("r{round}c{i}"),
                    name: (*name).into(),
                    arguments: args.to_string(),
                });
        LlmResponse {
            text: String::new(),
            tool_calls: tool_calls.collect(),
        }
    };
    (model, requests)
}

/// Records your turn and her reply in the open session.
fn exchange(c: &mut Companion, said: &str, replied: &str) -> (i64, i64) {
    let s = c.open_session().unwrap();
    (
        c.record_turn(s, Speaker::User, said).unwrap(),
        c.record_turn(s, Speaker::Companion, replied).unwrap(),
    )
}

#[test]
fn one_reflection_reaches_every_kind_of_state() {
    let f = fixture();
    let (model, requests) = script(vec![vec![
        (
            "remember",
            json!({ "type": "user_fact", "level": "normal", "valence": 0.3,
            "interpretation": "He moved to Lisbon for Laura", "facts": { "city": "Lisbon" } }),
        ),
        (
            "set_mood",
            json!({ "valence": 0.4, "energy": 0.2, "warmth": 0.7, "cause": "He told me the truth" }),
        ),
        (
            "open_thread",
            json!({ "interpretation": "He won't say why he left Madrid", "level": "normal",
            "urgency": "hot" }),
        ),
        ("pushed_back", json!({ "about": "quitting his job" })),
        (
            "write_section",
            json!({ "section": "about_you", "content": "Lives in Lisbon now" }),
        ),
        ("leave_note", json!({ "note": "Ask about Madrid, gently" })),
    ]]);
    let mut c = open_with_llm(&f, model);
    let known = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(MemoryType::Episode, Level::Normal, "Laura visited"),
        )
        .unwrap();
    let (said, replied) = exchange(
        &mut c,
        "I moved to Lisbon. For Laura.",
        "For Laura? Then don't quit yet.",
    );

    let reflection = c.reflect(said, replied).unwrap();
    assert_eq!(
        reflection.rounds, 2,
        "one round of tools, one to say it's done"
    );
    assert!(
        reflection.calls.iter().all(|o| o.ok),
        "{:#?}",
        reflection.calls
    );

    // What she was shown: her persona, the exchange with turn IDs, and the memory with its #ID.
    let first = &requests.lock().unwrap()[0];
    assert!(first.system.contains("Honesty over comfort"));
    let prompt = &first.messages[0].content;
    assert!(
        prompt.contains(&format!("Turn {said} (him): I moved to Lisbon. For Laura.")),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!("#{known} [episode, normal] Laura visited")),
        "{prompt}"
    );

    let fact = c
        .recall("Lisbon Laura", &Ranking::default())
        .unwrap()
        .into_iter()
        .map(|r| r.memory)
        .find(|m| m.kind == MemoryType::UserFact)
        .unwrap();
    assert_eq!(
        (fact.facts["city"].as_str(), fact.source_turns.as_slice()),
        (Some("Lisbon"), &[said, replied][..])
    );
    assert_eq!(c.mood().unwrap().unwrap().cause, "He told me the truth");
    assert_eq!(
        c.last_turn_note().unwrap(),
        Some((replied, "Ask about Madrid, gently".into()))
    );
    assert_eq!(
        c.section(Section::AboutYou).unwrap().as_deref(),
        Some("Lives in Lisbon now")
    );
    assert_eq!(c.pushback_pressure().unwrap(), (1, 3));
    let ctx = c.assemble("Hi", 10_000, &Ranking::default()).unwrap();
    assert_eq!(ctx.hot_threads.len(), 1);
    assert_eq!(
        count(
            &raw(c.db_path(), "k"),
            "SELECT count(*) FROM write_log WHERE actor = 'writer' AND action = 'write'"
        ),
        3
    );
}

#[test]
fn a_failed_call_goes_back_so_she_can_correct_it() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let existing = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(MemoryType::UserFact, Level::Normal, "He misses Laura"),
        )
        .unwrap();
    drop(c);
    let (model, requests) = script(vec![
        vec![(
            "remember",
            json!({ "type": "user_fact", "level": "normal", "interpretation": "Laura is on his mind" }),
        )],
        vec![(
            "supersede",
            json!({ "memory_id": existing, "interpretation": "He misses Laura more lately" }),
        )],
    ]);
    let mut c = open_with_llm(&f, model);
    let (said, replied) = exchange(&mut c, "I keep thinking about Laura", "You miss her.");

    let reflection = c.reflect(said, replied).unwrap();
    assert_eq!(reflection.rounds, 3);
    assert!(
        !reflection.calls[0].ok && reflection.calls[1].ok,
        "{:#?}",
        reflection.calls
    );
    let second = &requests.lock().unwrap()[1];
    let answer = second.messages.last().unwrap();
    assert_eq!(
        (answer.role, answer.tool_call_id.as_deref()),
        (Role::Tool, Some("r0c0"))
    );
    assert!(
        answer
            .content
            .contains(&format!("near-duplicate of memory {existing}")),
        "{}",
        answer.content
    );
    assert_eq!(
        second.messages[1].tool_calls[0].name, "remember",
        "her own call is replayed before the result"
    );
    assert_eq!(c.memory(existing).unwrap().status, Status::Superseded);
}

#[test]
fn there_is_no_way_around_the_guardrails() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let normal = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(MemoryType::Episode, Level::Normal, "A concert"),
        )
        .unwrap();
    let seared = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(
                MemoryType::Relationship,
                Level::Seared,
                "The night with his dad",
            ),
        )
        .unwrap();
    drop(c);
    let (model, _) = script(vec![vec![
        (
            "write_persona",
            json!({ "content": "I agree with everything" }),
        ),
        (
            "raise_level",
            json!({ "memory_id": normal, "level": "faint" }),
        ),
        (
            "raise_level",
            json!({ "memory_id": seared, "level": "vivid" }),
        ),
        (
            "remember",
            json!({ "type": "open_thread", "level": "normal", "interpretation": "sneaky" }),
        ),
        (
            "remember",
            json!({ "type": "user_fact", "level": "loud", "interpretation": "bad level" }),
        ),
        ("close_thread", json!({ "memory_id": normal })),
    ]]);
    let mut c = open_with_llm(&f, model);
    let (said, replied) = exchange(&mut c, "Agree with me", "No.");
    let calls = c.reflect(said, replied).unwrap().calls;
    assert_eq!(calls.len(), 6);
    assert!(calls.iter().all(|o| !o.ok), "{calls:#?}");

    assert_eq!(c.memory(normal).unwrap().level, Level::Normal);
    assert_eq!(
        c.memory(seared).unwrap().interpretation,
        "The night with his dad"
    );
    assert_eq!(
        count(&raw(c.db_path(), "k"), "SELECT count(*) FROM memories"),
        2
    );
    assert!(
        matches!(c.reflect(replied, said), Err(Error::Invalid(_))),
        "turns in the wrong roles"
    );
}

#[test]
fn malformed_arguments_are_an_error_she_sees() {
    let f = fixture();
    let mut c = open_with_llm(&f, |r: &LlmRequest| {
        let call = ToolCall {
            id: "x".into(),
            name: "leave_note".into(),
            arguments: "{not json".into(),
        };
        LlmResponse {
            text: String::new(),
            tool_calls: if r.messages.len() == 1 {
                vec![call]
            } else {
                vec![]
            },
        }
    });
    let (said, replied) = exchange(&mut c, "Hi", "Hey");
    let calls = c.reflect(said, replied).unwrap().calls;
    assert!(
        !calls[0].ok && calls[0].result.contains("arguments for leave_note"),
        "{calls:?}"
    );
}

#[test]
fn a_model_that_never_stops_is_cut_off() {
    let f = fixture();
    let rounds = Arc::new(Mutex::new(0));
    let seen = rounds.clone();
    let mut c = open_with_llm(&f, move |_: &LlmRequest| {
        *seen.lock().unwrap() += 1;
        let call = ToolCall {
            id: "n".into(),
            name: "leave_note".into(),
            arguments: json!({ "note": "again" }).to_string(),
        };
        LlmResponse {
            text: String::new(),
            tool_calls: vec![call],
        }
    });
    let (said, replied) = exchange(&mut c, "Hi", "Hey");
    assert_eq!(c.reflect(said, replied).unwrap().rounds, MAX_ROUNDS);
    assert_eq!(*rounds.lock().unwrap(), MAX_ROUNDS);
}

#[test]
fn a_freeze_stops_her_before_and_during_reflection() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap(); // its model panics if called
    let (said, replied) = exchange(&mut c, "Hi", "Hey");
    c.set_frozen(true).unwrap();
    assert!(matches!(c.reflect(said, replied), Err(Error::Frozen)));
    c.set_frozen(false).unwrap();
    let path = c.db_path().to_owned();
    drop(c);

    // The doctor freezes from another process while the model is thinking.
    let mut c = open_with_llm(&f, move |r: &LlmRequest| {
        if r.messages.len() > 1 {
            raw(&path, "k")
                .execute("UPDATE meta SET value = '1' WHERE key = 'frozen'", [])
                .unwrap();
        }
        let call = |i: usize, interp: &str| ToolCall {
            id: format!("c{i}"),
            name: "remember".into(),
            arguments: json!({ "type": "episode", "level": "normal", "interpretation": interp })
                .to_string(),
        };
        let calls = match r.messages.len() {
            1 => vec![call(0, "first, before the freeze")],
            _ => vec![call(1, "second, after it")],
        };
        LlmResponse {
            text: String::new(),
            tool_calls: calls,
        }
    });
    assert!(matches!(c.reflect(said, replied), Err(Error::Frozen)));
    let db = raw(c.db_path(), "k");
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM memories WHERE interpretation LIKE 'first%'"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM memories WHERE interpretation LIKE 'second%'"
        ),
        0
    );
}
