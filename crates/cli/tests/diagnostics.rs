mod common;

use clanka::diagnostics::{Log, Report};
use clanka::turn::{self, Begin};
use clanka_core::deps::{LlmResponse, ToolCall};
use common::{Scripted, open_noting};
use serde_json::json;

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
    let lower = log.to_lowercase();
    for word in ["marisol", "reykjavik", "sister", "escape", "matters"] {
        assert!(!lower.contains(word), "{word} is in the log:\n{log}");
    }
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
