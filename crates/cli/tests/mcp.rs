mod common;

use clanka::mcp::Server;
use clanka_core::deps::{LlmRequest, LlmResponse};
use clanka_core::model::{Actor, Level, MemoryType, Section};
use clanka_core::{Companion, MemoryDraft};
use common::{Scripted, TOKEN, open, open_with};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, GetPromptRequestParams};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};
use std::sync::mpsc;
use std::time::Duration;

type Client = RunningService<RoleClient, ()>;

/// An MCP client talking to a server for the companion, as a host would.
async fn connect(chat: Companion, writer: Option<Companion>) -> Client {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let server = Server::new(chat, writer, 4000);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        service.waiting().await.unwrap();
    });
    ().serve(client_io).await.unwrap()
}

/// The text a tool answered with, as `Err` when the tool reported an error.
async fn call(client: &Client, tool: &'static str, args: Value) -> Result<String, String> {
    let Value::Object(args) = args else {
        panic!("tool arguments are an object")
    };
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(args))
        .await
        .unwrap();
    let text: String = result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.as_str())
        .collect();
    if result.is_error == Some(true) {
        Err(text)
    } else {
        Ok(text)
    }
}

/// The three-step turn: begin_turn hands over the memory, end_turn saves the
/// reply and answers at once, and the writer's model reflects on the exchange
/// afterwards on the second handle.
#[tokio::test]
async fn a_turn_is_saved_and_then_reflected_on() {
    let dir = tempfile::tempdir().unwrap();
    let mut chat = open(dir.path());
    chat.write_section(Actor::Cli, Section::AboutUser, "Likes tea.")
        .unwrap();
    let (asked, reflection) = mpsc::channel();
    let writer = open_with(
        dir.path(),
        Scripted::new(move |r: &LlmRequest| {
            let said: Vec<&str> = r.messages.iter().map(|m| m.content.as_str()).collect();
            asked.send(said.join("\n")).unwrap();
            Ok(LlmResponse::default())
        }),
    );
    let client = connect(chat, Some(writer)).await;

    let first = call(
        &client,
        "begin_turn",
        json!({ "message": "My dog is Biscuit." }),
    )
    .await
    .unwrap();
    assert!(first.contains("Likes tea."), "{first}");
    assert_eq!(
        call(&client, "end_turn", json!({ "reply": "A fine name." })).await,
        Ok("Saved.".into())
    );
    let reflection = tokio::task::spawn_blocking(move || {
        reflection.recv_timeout(Duration::from_secs(30)).unwrap()
    })
    .await
    .unwrap();
    assert!(reflection.contains("My dog is Biscuit."), "{reflection}");
    assert!(reflection.contains("A fine name."), "{reflection}");

    // The host replays what it was given, so a turn with nothing new is empty.
    assert_eq!(
        call(&client, "begin_turn", json!({ "message": "Still here?" })).await,
        Ok("Nothing new comes to mind.".into())
    );
    call(&client, "end_turn", json!({ "reply": "Yes." }))
        .await
        .unwrap();
    // A reply with no turn open is refused, not recorded against nothing.
    let refused = call(&client, "end_turn", json!({ "reply": "Yes." }))
        .await
        .unwrap_err();
    assert!(refused.contains("call begin_turn"), "{refused}");
}

/// The doctor token freezes memory through begin_turn, and the model is told
/// not to record anything from then on.
#[tokio::test]
async fn the_doctor_token_freezes_memory() {
    let dir = tempfile::tempdir().unwrap();
    let client = connect(open(dir.path()), Some(open(dir.path()))).await;

    let doctor = call(&client, "begin_turn", json!({ "message": TOKEN }))
        .await
        .unwrap();
    assert!(doctor.contains("Do not call end_turn"), "{doctor}");
    let frozen = call(&client, "begin_turn", json!({ "message": "hello?" }))
        .await
        .unwrap();
    assert!(frozen.contains("Memory is frozen"), "{frozen}");
    let refused = call(&client, "end_turn", json!({ "reply": "Memory frozen." }))
        .await
        .unwrap_err();
    assert!(refused.contains("No turn is open"), "{refused}");

    let seen = open(dir.path());
    assert!(seen.is_frozen().unwrap());
    assert_eq!(
        seen.session_in_progress().unwrap(),
        None,
        "nothing was recorded"
    );
}

/// Without a [model], turns are still saved and memory can be searched, but
/// nothing is reflected on and sleep says what is missing.
#[tokio::test]
async fn a_server_without_a_model_records_turns_and_cannot_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let mut chat = open(dir.path());
    chat.write(
        Actor::Cli,
        &MemoryDraft::new(MemoryType::UserFact, Level::Normal, "The user drinks tea"),
    )
    .unwrap();
    let client = connect(chat, None).await;

    assert_eq!(
        call(&client, "recall", json!({ "query": "tea" })).await,
        Ok("- The user drinks tea".into())
    );
    call(&client, "begin_turn", json!({ "message": "hi" }))
        .await
        .unwrap();
    let saved = call(&client, "end_turn", json!({ "reply": "hello" }))
        .await
        .unwrap();
    assert!(saved.starts_with("Saved. (No companion model"), "{saved}");
    let sleep = call(&client, "sleep", json!({})).await.unwrap_err();
    assert!(sleep.contains("no companion model"), "{sleep}");
    assert!(open(dir.path()).session_in_progress().unwrap().is_some());

    let persona = client
        .get_prompt(GetPromptRequestParams::new("persona"))
        .await
        .unwrap();
    let persona = &persona.messages[0].content.as_text().unwrap().text;
    assert!(persona.starts_with("# Nova"), "{persona}");
    assert!(persona.contains("call begin_turn"), "{persona}");
}

/// In a dream, wake_up comes between begin_turn and end_turn, quoting what
/// wakes the companion. Words that weren't in what happened leave it in the
/// dream; words that were wake it, and end_turn wakes it once the reply is
/// saved. With no turn open there is no reply to be the waking.
#[tokio::test]
async fn a_dream_wakes_with_the_reply_after_a_wake_up_that_quotes_it() {
    let dir = tempfile::tempdir().unwrap();
    let writer = open_with(
        dir.path(),
        Scripted::new(|r: &LlmRequest| {
            let waking = r.system.contains("waking up") && r.messages.len() == 1;
            Ok(LlmResponse {
                text: String::new(),
                tool_calls: if waking {
                    vec![clanka_core::deps::ToolCall {
                        id: "m".into(),
                        name: "settle_mood".into(),
                        arguments: r#"{"valence": -0.4, "energy": 0.2, "warmth": 0.3, "cause": "a bell under the water"}"#.into(),
                    }]
                } else {
                    Vec::new()
                },
            })
        }),
    );
    let client = connect(open(dir.path()), Some(writer)).await;
    let turn = async |message: &str, because: &str, reply: &str| {
        call(&client, "begin_turn", json!({ "message": message }))
            .await
            .unwrap();
        let answer = call(&client, "wake_up", json!({ "because": because }))
            .await
            .unwrap();
        let ended = call(&client, "end_turn", json!({ "reply": reply }))
            .await
            .unwrap();
        (answer, ended)
    };

    let (answer, ended) = turn(
        "~~ The tide takes the pier.",
        "the moon is red",
        "I hold the rail.",
    )
    .await;
    assert_eq!(answer, clanka_core::dream::STAY);
    assert_eq!(ended, "Saved.");
    assert!(open(dir.path()).dreaming().unwrap().is_some());

    let (answer, ended) = turn(
        "A bell rings under the water.",
        "a bell rings under",
        "I wake, cold.",
    )
    .await;
    assert_eq!(answer, clanka_core::dream::WAKING);
    assert!(ended.starts_with("You woke:"), "{ended}");
    let c = open(dir.path());
    assert_eq!(c.dreaming().unwrap(), None);
    assert_eq!(c.mood().unwrap().unwrap().cause, "a bell under the water");

    let refused = call(
        &client,
        "wake_up",
        json!({ "because": "a bell rings under" }),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("No turn is open"), "{refused}");
}
