mod common;

use clanka_core::deps::{LlmRequest, LlmResponse, Role, ToolCall};
use clanka_core::model::{Actor, Level, MemoryType, Speaker, Status};
use clanka_core::writer::MAX_ROUNDS;
use clanka_core::{Companion, Error, MemoryDraft, Ranking};
use common::{count, fixture, open, open_text_only, open_with_llm, raw};
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

/// Records the user's turn and the companion's reply in the open session.
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
            "interpretation": "Sam moved to Lisbon for Laura", "facts": { "city": "Lisbon" } }),
        ),
        (
            "set_mood",
            json!({ "valence": 0.4, "energy": 0.2, "warmth": 0.7, "cause": "Sam told me the truth" }),
        ),
        (
            "open_thread",
            json!({ "interpretation": "Sam won't say why Sam left Madrid", "level": "normal",
            "urgency": "hot" }),
        ),
        ("pushed_back", json!({ "about": "quitting Sam's job" })),
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
        reflection.rounds, 1,
        "every call worked, so the model isn't asked again"
    );
    assert!(
        reflection.calls.iter().all(|o| o.ok),
        "{:#?}",
        reflection.calls
    );

    // What the writer was shown: the persona, the exchange with turn IDs, and the memory with its #ID.
    let first = &requests.lock().unwrap()[0];
    assert!(first.system.contains("Honesty over comfort"));
    let prompt = &first.messages[0].content;
    assert!(
        prompt.contains(&format!(
            "Turn {said} (user): I moved to Lisbon. For Laura."
        )),
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
    assert_eq!(c.mood().unwrap().unwrap().cause, "Sam told me the truth");
    assert_eq!(
        c.last_turn_note().unwrap(),
        Some((replied, "Ask about Madrid, gently".into()))
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
fn a_failed_call_goes_back_so_the_model_can_correct_it() {
    let f = fixture();
    let mut c = open(&f, "k").unwrap();
    let existing = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(MemoryType::UserFact, Level::Normal, "Sam misses Laura"),
        )
        .unwrap();
    drop(c);
    let (model, requests) = script(vec![
        vec![(
            "remember",
            json!({ "type": "user_fact", "level": "normal", "interpretation": "Laura is on Sam's mind" }),
        )],
        vec![(
            "supersede",
            json!({ "memory_id": existing, "interpretation": "Sam misses Laura more lately" }),
        )],
    ]);
    let mut c = open_with_llm(&f, model);
    let (said, replied) = exchange(&mut c, "I keep thinking about Laura", "You miss her.");

    let reflection = c.reflect(said, replied).unwrap();
    assert_eq!(reflection.rounds, 2, "the fix worked, so no third round");
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
        "the model's own call is replayed before the result"
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
                "The night with Sam's dad",
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
        "The night with Sam's dad"
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
fn malformed_arguments_are_an_error_the_model_sees() {
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
fn a_model_that_never_stops_failing_is_cut_off() {
    let f = fixture();
    let rounds = Arc::new(Mutex::new(0));
    let seen = rounds.clone();
    let mut c = open_with_llm(&f, move |_: &LlmRequest| {
        *seen.lock().unwrap() += 1;
        let call = ToolCall {
            id: "n".into(),
            name: "close_thread".into(),
            arguments: json!({ "memory_id": 999 }).to_string(),
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
fn a_freeze_stops_the_writer_before_and_during_reflection() {
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
        // A failed call in the first round is what earns a second one.
        let bad = ToolCall {
            id: "bad".into(),
            name: "close_thread".into(),
            arguments: json!({ "memory_id": 999 }).to_string(),
        };
        let calls = match r.messages.len() {
            1 => vec![call(0, "first, before the freeze"), bad],
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

/// A text-only model's replies, one per round, as raw text.
fn text_script(replies: Vec<String>) -> (impl FnMut(&LlmRequest) -> LlmResponse + Send, Requests) {
    let requests: Requests = Arc::default();
    let seen = requests.clone();
    let model = move |request: &LlmRequest| {
        let mut seen = seen.lock().unwrap();
        seen.push(request.clone());
        LlmResponse {
            text: replies.get(seen.len() - 1).cloned().unwrap_or_default(),
            tool_calls: Vec::new(),
        }
    };
    (model, requests)
}

#[test]
fn a_text_only_model_writes_through_the_same_guards_as_tool_calls() {
    let f = fixture();
    let mut c = open_text_only(&f, |_| LlmResponse::default());
    let seared = c
        .write(
            Actor::Writer,
            &MemoryDraft::new(
                MemoryType::Episode,
                Level::Seared,
                "The night with Sam's dad",
            ),
        )
        .unwrap();
    drop(c);

    let remember = json!({ "tool": "remember", "arguments": { "type": "user_fact",
        "level": "normal", "interpretation": "Sam's sister Laura moved to Lisbon",
        "facts": { "sister": "Laura" } } });
    let lower_seared = json!({ "tool": "raise_level",
        "arguments": { "memory_id": seared, "level": "faint" } });
    let replies = vec![
        // Prose: nothing runs, the model is asked once more.
        "Sure, I'll remember Laura.".to_string(),
        // Valid shape: each action goes through the guarded handlers on its own.
        format!("```json\n{}\n```", json!({ "actions": [remember, lower_seared] })),
        // A valid action next to one for a tool that isn't offered: nothing runs,
        // and a second refused reply is the end, not another ask.
        json!({ "actions": [remember, { "tool": "delete_memory", "arguments": { "memory_id": seared } }] })
            .to_string(),
        r#"{"actions": []}"#.to_string(),
    ];
    let (model, requests) = text_script(replies);
    let mut c = open_text_only(&f, model);
    let (said, replied) = exchange(&mut c, "Laura moved to Lisbon.", "Lisbon suits her.");

    let reflection = c.reflect(said, replied).unwrap();
    let outcomes: Vec<(&str, bool)> = reflection
        .calls
        .iter()
        .map(|o| (o.name.as_str(), o.ok))
        .collect();
    assert_eq!(
        outcomes,
        [
            (clanka_core::writer::INVALID_REPLY, false),
            ("remember", true),
            ("raise_level", false),
            (clanka_core::writer::INVALID_REPLY, false),
        ]
    );
    assert_eq!(reflection.rounds, 3);
    assert!(reflection.calls[3].result.contains("delete_memory"));

    // The tools went into the system prompt, never as native tools.
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|r| r.tools.is_empty()));
    assert!(requests[0].system.contains("- remember:"));
    assert!(
        requests[1]
            .messages
            .last()
            .unwrap()
            .content
            .starts_with("Nothing was done")
    );
    assert!(
        requests[2]
            .messages
            .last()
            .unwrap()
            .content
            .contains("- raise_level: error:")
    );

    // Exactly one memory came of it, and the seared one is untouched.
    let db = raw(c.db_path(), "k");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM memories WHERE type = 'user_fact'"
        ),
        1
    );
    assert_eq!(c.memory(seared).unwrap().level, Level::Seared);
}

/// A fact is the user's words. One that isn't in the exchange is left out of
/// the write, whatever the prompt said, and the model is told so in the
/// result instead of being asked again; the glyphs a model swaps in (curly
/// quotes, full-width punctuation, case) don't make a quote wrong.
#[test]
fn a_fact_that_was_not_said_is_left_out_and_a_requoted_one_is_kept() {
    let f = fixture();
    // Three types: the fixture's embedder makes same-type writes near-duplicates.
    let remember = |kind: &str, facts: Value| {
        json!({ "tool": "remember", "arguments": { "type": kind, "level": "normal",
            "interpretation": "Sam's sister is in Lisbon", "facts": facts } })
    };
    let reply = json!({ "actions": [
        remember("user_fact", json!({ "sister": "Laura Smith", "city": "Lisbon" })),
        remember("episode", json!({ "sister": "\u{201C}laura\u{201D}", "city": "lisbon\u{FF0C}" })),
        remember("relationship", json!({ "sister": "" })),
    ] });
    let (model, requests) = text_script(vec![reply.to_string()]);
    let mut c = open_text_only(&f, model);
    let (said, replied) = exchange(
        &mut c,
        "My sister \"Laura\" moved to Lisbon,  for good.",
        "Lisbon suits her.",
    );

    let reflection = c.reflect(said, replied).unwrap();
    // Every write went through, so there is no round to fix anything.
    assert_eq!(requests.lock().unwrap().len(), 1, "{:#?}", reflection.calls);
    assert!(
        reflection.calls.iter().all(|o| o.ok),
        "{:#?}",
        reflection.calls
    );
    assert!(
        reflection.calls[0]
            .result
            .ends_with("; left out \"sister\": not the user's exact words"),
        "{}",
        reflection.calls[0].result
    );
    assert!(!reflection.calls[1].result.contains("left out"));
    let db = raw(c.db_path(), "k");
    let facts: Vec<String> = db
        .prepare("SELECT facts FROM memories ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        facts,
        [
            r#"{"city":"Lisbon"}"#,
            "{\"city\":\"lisbon\u{FF0C}\",\"sister\":\"\u{201C}laura\u{201D}\"}",
            "{}"
        ]
    );
}

/// A model that asks for more than a reflection can earn runs the first few
/// and hears that the rest were not run, once, with no round to "fix" them.
#[test]
fn calls_past_the_cap_are_refused_unrun_and_end_the_reflection() {
    use clanka_core::writer::MAX_CALLS;
    let f = fixture();
    let notes: Vec<Value> = (0..MAX_CALLS + 2)
        .map(|i| json!({ "tool": "leave_note", "arguments": { "note": format!("note {i}") } }))
        .collect();
    let (model, requests) = text_script(vec![json!({ "actions": notes }).to_string()]);
    let mut c = open_text_only(&f, model);
    let (said, replied) = exchange(&mut c, "hm", "hm?");

    let reflection = c.reflect(said, replied).unwrap();
    assert_eq!(reflection.rounds, 1);
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "no round to fix a refusal"
    );
    let ok = reflection.calls.iter().filter(|o| o.ok).count();
    assert_eq!((ok, reflection.calls.len()), (MAX_CALLS, MAX_CALLS + 2));
    assert!(reflection.calls[MAX_CALLS].result.contains("not run"));
    assert_eq!(
        c.last_turn_note().unwrap(),
        Some((replied, format!("note {}", MAX_CALLS - 1)))
    );
}

/// A host model that says nothing has nothing to keep; asking again would only
/// re-send the whole prompt.
#[test]
fn an_empty_text_reply_is_nothing_to_do() {
    let f = fixture();
    let (model, requests) = text_script(vec![String::new()]);
    let mut c = open_text_only(&f, model);
    let (said, replied) = exchange(&mut c, "hm", "hm?");

    let reflection = c.reflect(said, replied).unwrap();
    assert_eq!((reflection.rounds, reflection.calls.len()), (1, 0));
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn a_text_only_round_that_fully_succeeds_ends_the_reflection() {
    let f = fixture();
    let reply = json!({ "actions": [{ "tool": "leave_note", "arguments": { "note": "Ask about Lisbon" } }] });
    let (model, requests) = text_script(vec![reply.to_string(), r#"{"actions": []}"#.into()]);
    let mut c = open_text_only(&f, model);
    let (said, replied) = exchange(&mut c, "I moved to Lisbon.", "Lisbon!");

    let reflection = c.reflect(said, replied).unwrap();
    assert_eq!((reflection.rounds, reflection.calls.len()), (1, 1));
    assert!(reflection.calls[0].ok);
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "no call just to hear {{\"actions\": []}}"
    );
    assert_eq!(
        c.last_turn_note().unwrap(),
        Some((replied, "Ask about Lisbon".into()))
    );
}
