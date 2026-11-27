mod common;

use clanka::diagnostics::{Log, Report, error_code};
use clanka::setup::{self, Config, Keys, Paths};
use clanka::turn::{self, Begin};
use clanka_core::deps::{LlmResponse, ToolCall};
use common::{Dummy, Scripted, open, open_noting};
use serde_json::{Value, json};
use std::path::Path;

/// The log's lines, each as the JSON it is.
fn lines(file: &Path) -> Vec<Value> {
    let log = std::fs::read_to_string(file).unwrap();
    log.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// An evening with a model that misbehaves in every way the log should show:
/// the turns, the reflection's refused calls and the sleep that fails are all
/// counted, and nothing anyone said, remembered or made up is in the file.
#[test]
fn an_evening_is_counted_and_nothing_said_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("diagnostics.jsonl");
    let call = |name: &str, arguments: serde_json::Value| ToolCall {
        id: name.to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_string(),
    };
    let mut asked = 0;
    let model = Scripted::new(move |_| {
        asked += 1;
        Ok(LlmResponse {
            text: "Marisol matters to them.".into(),
            tool_calls: match asked {
                // The reflection: a memory, a call that can't run and a tool
                // that doesn't exist, named after what was said.
                1 => vec![
                    call(
                        "remember",
                        json!({ "type": "user_fact", "level": "normal",
                            "interpretation": "Their sister Marisol moved to Reykjavik",
                            "facts": { "sister": "Marisol" } }),
                    ),
                    call(
                        "raise_level",
                        json!({ "memory_id": 4040, "level": "vivid" }),
                    ),
                    call("marisol_in_reykjavik", json!({ "note": "Marisol" })),
                ],
                // Told what failed, it leaves it. Asked to sleep, it writes
                // no episode.
                _ => Vec::new(),
            },
        })
    });
    let mut c = open_noting(dir.path(), model, Log::to(file.clone()));

    let said = "My sister Marisol moved to Reykjavik";
    let Begin::Open { pending, .. } = turn::begin(&mut c, said, 4000, true).unwrap() else {
        panic!("the turn did not open");
    };
    let reply = "Reykjavik \u{2014} that far? It isn't a move. It's an escape.";
    let (user, companion) = turn::end(&mut c, pending, reply).unwrap();
    c.reflect(user, companion).unwrap();
    assert!(turn::recall(&c, "Marisol").is_ok());
    let session = c.session_in_progress().unwrap().unwrap();
    assert!(c.sleep(session).is_err());

    let log = std::fs::read_to_string(&file).unwrap();
    // `/triage` shows the evening from the log, and no more of it than the log.
    let triage = clanka::triage::report(&c, Some(&file)).unwrap();
    for (text, what) in [(&log, "log"), (&triage, "triage")] {
        let lower = text.to_lowercase();
        for word in ["marisol", "reykjavik", "sister", "escape", "matters"] {
            assert!(!lower.contains(word), "{word} is in the {what}:\n{text}");
        }
    }
    assert!(
        triage.contains("Now: no conversation open; 1 night waits to be slept on."),
        "{triage}"
    );
    assert!(
        triage.contains(" memory_pass.reflect ")
            && triage.contains("refused=raise_level/not_found,unknown/invalid")
            && triage.contains(" memory_pass.sleep error=llm"),
        "{triage}"
    );
    // Every line names the companion by the end of its ID, and nothing else.
    let id = c.id().simple().to_string();
    assert!(!log.contains(&id), "{log}");
    assert_eq!(log.matches(&id[24..]).count(), log.lines().count(), "{log}");

    let report = Report::read(&file, None).unwrap();
    assert_eq!(report.unreadable, 0);
    assert_eq!(report.companions, 1);

    let begun = &report.kinds["turn_begin"];
    assert_eq!(begun.words["outcome"]["open"], 1);
    assert_eq!(begun.flags["first"], 1);
    assert_eq!(
        begun.numbers["message_chars"].max,
        said.chars().count() as f64
    );

    let ended = &report.kinds["turn_end"];
    assert_eq!(ended.flags["dashes"], 1);
    assert_eq!(ended.numbers["tics.negation"].max, 1.0);
    assert!(ended.numbers["reply_chars"].max > 0.0);

    // The reflection kept one memory; the two other calls are named by the
    // tool clanka offers and the kind of error, and the made-up tool by neither.
    let reflection = &report.kinds["memory_pass.reflect"];
    assert_eq!(reflection.numbers["ok"].max, 1.0);
    assert_eq!(reflection.numbers["rounds"].max, 2.0);
    assert_eq!(reflection.words["refused"]["raise_level/not_found"], 1);
    assert_eq!(reflection.words["refused"]["unknown/invalid"], 1);
    assert!(!reflection.words.contains_key("error"));
    assert_eq!(report.kinds["model_call.reflect"].count, 2);

    assert_eq!(report.kinds["recall"].numbers["found"].max, 1.0);

    // The night failed, and the log says which way.
    let night = &report.kinds["memory_pass.sleep"];
    assert_eq!(night.words["error"]["llm"], 1);
    assert_eq!(report.kinds["model_call.sleep"].count, 1);
}

/// A message whose reply never came (the host's model failed, or the host
/// never said what it answered) leaves no event of its own: the next turn
/// says so.
#[test]
fn a_message_that_got_no_reply_shows_on_the_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("diagnostics.jsonl");
    let mut c = open_noting(dir.path(), Dummy, Log::to(file.clone()));
    let mut begin = |said: &str, reply: Option<&str>| {
        let Begin::Open { pending, .. } = turn::begin(&mut c, said, 4000, false).unwrap() else {
            panic!("the turn did not open");
        };
        if let Some(reply) = reply {
            turn::end(&mut c, pending, reply).unwrap();
        }
    };
    begin("Are you there?", Some("Here."));
    begin("And now?", None);
    begin("Hello?", None);

    let unanswered: Vec<Value> = lines(&file)
        .into_iter()
        .filter(|line| line["event"] == "turn_begin")
        .map(|line| line["unanswered"].clone())
        .collect();
    assert_eq!(unanswered, [json!(false), json!(false), json!(true)]);
}

/// A reflection refused before it calls the model is a pass that failed, and
/// the last exchange of a conversation has no later turn to show it on.
#[test]
fn a_reflection_refused_before_its_model_call_is_a_failed_pass() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("diagnostics.jsonl");
    let mut c = open_noting(dir.path(), Dummy, Log::to(file.clone()));
    let Begin::Open { pending, .. } = turn::begin(&mut c, "Goodbye.", 4000, true).unwrap() else {
        panic!("the turn did not open");
    };
    let (user, companion) = turn::end(&mut c, pending, "Until tomorrow.").unwrap();
    c.set_frozen(true).unwrap();
    assert!(c.reflect(user, companion).is_err());

    let passes: Vec<Value> = lines(&file)
        .into_iter()
        .filter(|line| line["event"] == "memory_pass")
        .collect();
    assert_eq!(passes.len(), 1, "{passes:?}");
    assert_eq!(passes[0]["pass"], "reflect");
    assert_eq!(passes[0]["error"], "frozen");
    assert_eq!(passes[0]["rounds"], 0);
}

/// The reply model's `go_to_sleep` calls are in the log, with whether the
/// user's `zz` had ended the night already or the model chose to sleep.
#[test]
fn a_go_to_sleep_call_is_noted_with_why() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("diagnostics.jsonl");
    let mut c = open_noting(dir.path(), Dummy, Log::to(file.clone()));
    let call = |c: &mut clanka_core::Companion, said: &str| {
        let Begin::Open { pending, .. } = turn::begin(c, said, 4000, false).unwrap() else {
            panic!("the turn did not open");
        };
        turn::go_to_sleep(c, Some(&pending));
        turn::end(c, pending, "Night.").unwrap();
    };
    call(&mut c, "I'm off to bed.");
    call(&mut c, "zz");
    call(&mut c, "~~ A lighthouse with no door.");
    call(&mut c, "zz");
    turn::go_to_sleep(&c, None);

    let calls: Vec<Value> = lines(&file)
        .into_iter()
        .filter(|line| line["event"] == "go_to_sleep")
        .map(|line| json!(["ending", "dream", "unopened"].map(|flag| line[flag].clone())))
        .collect();
    assert_eq!(
        calls,
        [
            json!([false, false, false]),
            json!([true, false, false]),
            json!([false, true, false]),
            json!([true, true, false]),
            json!([false, false, true]),
        ]
    );
}

/// The reply model's `wake_up` calls are in the log, refused or not. On the
/// reply a dream ends with anyway, the model is told it wakes and quotes that
/// line: the answer is that it wakes, not that it is still there.
#[test]
fn a_wake_up_call_is_noted_and_never_contradicts_a_dream_that_is_ending() {
    use clanka_core::dream::{STAY, WAKING, WAKING_HINT};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("diagnostics.jsonl");
    let mut c = open_noting(dir.path(), Dummy, Log::to(file.clone()));
    let begin = |c: &mut clanka_core::Companion, said: &str| match turn::begin(c, said, 4000, false)
        .unwrap()
    {
        Begin::Open { context, pending } => (context, pending),
        other => panic!("the turn did not open: {other:?}"),
    };

    // Something it made up wakes nothing, and the dream goes on.
    let (_, mut pending) = begin(&mut c, "~~ The stairs go down into the sea.");
    let made_up = turn::wake_up(&c, &mut pending, "the tide takes the house").unwrap();
    assert_eq!(made_up, (false, STAY));
    turn::end(&mut c, pending, "I go down.").unwrap();

    // Goodnight in a dream ends it with this reply, as a full session does.
    let (told, mut pending) = begin(&mut c, "zz");
    assert!(told.contains(WAKING_HINT) && pending.wake, "{told}");
    let quoted_the_hint = turn::wake_up(&c, &mut pending, WAKING_HINT).unwrap();
    assert_eq!(quoted_the_hint, (true, WAKING));
    // A call after the turn ended has nothing to wake from; its front end
    // says so, and the log still has it.
    turn::end(&mut c, pending, "I wake.").unwrap();
    turn::wake_up_without_turn(&c, "wake me now");

    let calls: Vec<Value> = lines(&file)
        .into_iter()
        .filter(|line| line["event"] == "wake_up")
        .map(|line| {
            let flags = ["wakes", "ending", "unopened"].map(|flag| line[flag].clone());
            json!([flags, line["quoted_words"]])
        })
        .collect();
    assert_eq!(
        calls,
        [
            json!([[false, false, false], 5]),
            json!([[true, true, false], 10]),
            json!([[false, false, true], 3]),
        ]
    );
    // The `zz` that woke the dream is logged as a goodnight, which tells it
    // from a dream that filled its session.
    let begun: Vec<Value> = lines(&file)
        .into_iter()
        .filter(|line| line["event"] == "turn_begin")
        .map(|line| json!([line["dream"], line["goodnight"], line["spent"]]))
        .collect();
    assert_eq!(
        begun,
        [json!([true, false, false]), json!([true, true, true])]
    );
    // `/triage` tells that zz ended the dream: a dream's goodnight, spent.
    let triage = clanka::triage::report(&c, Some(&file)).unwrap();
    assert!(
        triage.lines().any(|row| {
            row.contains(" turn_begin ")
                && [" dream", " goodnight", " spent"]
                    .iter()
                    .all(|flag| row.contains(flag))
        }),
        "{triage}"
    );
    assert!(
        clanka::triage::report(&c, None).unwrap().ends_with(
            "Diagnostics are off ([diagnostics] enabled = false), so there are no events."
        ),
    );
}

/// An open that fails is still about a companion, and one refused for a
/// changed persona says so: without either, a blocked turn in the log could
/// not be tied to a companion or a cause.
#[test]
fn a_failed_open_names_its_companion_and_why() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        config: dir.path().join("config"),
        souls: dir.path().join("config/souls"),
        data: dir.path().join("data"),
        models: dir.path().join("data/models"),
    };
    std::fs::create_dir_all(&paths.souls).unwrap();
    std::fs::write(paths.config_file(), "[keys]\nstore = \"file\"\n").unwrap();
    // A soul.md where `clanka init` puts it, named after its ID.
    let id = open(dir.path()).id();
    let soul = paths.souls.join(format!("{id}.md"));
    std::fs::rename(dir.path().join("soul.md"), &soul).unwrap();
    Keys::new(&paths, &Config::load(&paths).unwrap())
        .store(id, "k")
        .unwrap();
    let to_talk = || setup::open_to_talk(&paths, &soul, Dummy, Box::new(Dummy));

    to_talk().unwrap();
    to_talk().unwrap();
    let persona = std::fs::read_to_string(&soul).unwrap();
    std::fs::write(&soul, format!("{persona}\nShe has changed.\n")).unwrap();
    let Err(changed) = to_talk() else {
        panic!("a changed persona was talked with");
    };
    assert_eq!(error_code(&changed), "persona_unresolved");
    // `clanka soul continue` and the doctor still open it.
    setup::open_with(&paths, &soul, Dummy, Box::new(Dummy)).unwrap();
    std::fs::write(&soul, "not a soul").unwrap();
    assert_eq!(error_code(&to_talk().err().unwrap()), "soul");
    std::fs::remove_file(&soul).unwrap();
    assert_eq!(error_code(&to_talk().err().unwrap()), "io");

    let opens = lines(&clanka::diagnostics::Log::file(&paths));
    let short = id.simple().to_string()[24..].to_owned();
    assert!(
        opens
            .iter()
            .all(|open| open["event"] == "open" && open["companion"] == short)
    );
    let seen: Vec<(Value, Value)> = opens
        .iter()
        .map(|open| (open["created"].clone(), open["error"].clone()))
        .collect();
    assert_eq!(
        seen,
        [
            (json!(true), Value::Null),
            (json!(false), Value::Null),
            (json!(false), json!("persona_unresolved")),
            (json!(false), Value::Null),
            (json!(false), json!("soul")),
            (json!(false), json!("io")),
        ]
    );

    // A config.toml with an error is its own kind, which the Hermes plugin
    // tells the user.
    std::fs::write(paths.config_file(), "[keys]\nstore = \"fiel\"\n").unwrap();
    assert_eq!(error_code(&to_talk().err().unwrap()), "config_invalid");
}

/// Lore a companion reads with an error in it refuses the companion to a
/// front end, as `clanka lore lint` would, and lore it doesn't read doesn't.
#[test]
fn lore_with_an_error_it_reads_refuses_the_companion() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        config: dir.path().join("config"),
        souls: dir.path().join("config/souls"),
        data: dir.path().join("data"),
        models: dir.path().join("data/models"),
    };
    std::fs::create_dir_all(&paths.souls).unwrap();
    std::fs::write(paths.config_file(), "[keys]\nstore = \"file\"\n").unwrap();
    let id = open(dir.path()).id();
    let soul = paths.souls.join(format!("{id}.md"));
    // A grant is frontmatter, not persona: no settling needed.
    let text = std::fs::read_to_string(dir.path().join("soul.md")).unwrap();
    std::fs::write(
        &soul,
        text.replacen("doctor_token", "lore = [\"w/\"]\ndoctor_token", 1),
    )
    .unwrap();
    Keys::new(&paths, &Config::load(&paths).unwrap())
        .store(id, "k")
        .unwrap();
    let lore = paths.config.join("lore");
    let entry = |path: &str, title: &str, alias: &str| {
        let file = lore.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            file,
            format!("+++\ntitle = \"{title}\"\naliases = [\"{alias}\"]\n+++\nText.\n"),
        )
        .unwrap();
    };
    let to_talk = || setup::open_to_talk(&paths, &soul, Dummy, Box::new(Dummy));

    entry("w/barrier.md", "The Barrier", "barrier");
    // Two entries it doesn't read share a name, and one is broken: not its lore.
    entry("other/a.md", "A", "shared");
    entry("other/b.md", "B", "shared");
    std::fs::write(lore.join("other/broken.md"), "no frontmatter").unwrap();
    to_talk().unwrap();

    // Two entries it reads share a name: a message that says it opens both.
    entry("w/ward.md", "The Ward", "barrier");
    let Err(refused) = to_talk() else {
        panic!("lore with a shared name was talked with");
    };
    assert_eq!(error_code(&refused), "lore_invalid");
    assert!(
        refused.to_string().contains("w/barrier, w/ward"),
        "{refused}"
    );
    // The doctor and `clanka soul` still open it.
    setup::open_with(&paths, &soul, Dummy, Box::new(Dummy)).unwrap();

    std::fs::remove_file(lore.join("w/ward.md")).unwrap();
    std::fs::write(lore.join("w/barrier.md"), "+++\ntitle = \n").unwrap();
    assert_eq!(error_code(&to_talk().err().unwrap()), "lore_invalid");
    entry("w/barrier.md", "The Barrier", "barrier");
    to_talk().unwrap();
}
