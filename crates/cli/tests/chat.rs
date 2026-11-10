mod common;

use clanka::chat::converse;
use clanka_core::Error;
use clanka_core::deps::{LlmRequest, LlmResponse, Role};
use common::{Scripted, TOKEN, open, open_with};
use std::io::Write;
use std::sync::{Arc, Mutex};

/// What the chat model was asked, as the roles and texts of each request.
type Asked = Arc<Mutex<Vec<Vec<(Role, String)>>>>;

/// A chat model that answers with `replies` in order and keeps each request.
fn scripted(replies: Vec<Result<&'static str, &'static str>>) -> (Scripted, Asked) {
    let asked = Asked::default();
    let (seen, mut replies) = (asked.clone(), replies.into_iter());
    let model = Scripted::new(move |r: &LlmRequest| {
        let said = r.messages.iter().map(|m| (m.role, m.content.clone()));
        seen.lock().unwrap().push(said.collect());
        match replies.next().expect("a reply for every request") {
            Ok(text) => Ok(LlmResponse {
                text: text.into(),
                tool_calls: Vec::new(),
            }),
            Err(e) => Err(Error::Llm(e.into())),
        }
    });
    (model, asked)
}

/// The notes stream, kept so a test can read it after the chat ends.
#[derive(Clone, Default)]
struct Notes(Arc<Mutex<Vec<u8>>>);
impl Notes {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}
impl Write for Notes {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A terminal that went away: every write fails.
struct Closed;
impl Write for Closed {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

/// A message whose reply failed stays on record with no reply after it. The
/// next request carries it joined to the next message, so the model never gets
/// two user messages in a row, and the history after that alternates again.
#[test]
fn a_message_whose_reply_failed_goes_out_with_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let (model, asked) = scripted(vec![Err("503"), Ok("\n\n I'm here."), Ok("Always.")]);
    let chat = open_with(dir.path(), model);
    let writer = open(dir.path());
    let (mut out, notes) = (Vec::new(), Notes::default());
    converse(
        chat,
        writer,
        4000,
        "my dad died\n\nare you there?\nthanks\n".as_bytes(),
        &mut out,
        notes.clone(),
    )
    .unwrap();

    let user = |text: &str| (Role::User, text.to_owned());
    let nova = |text: &str| (Role::Assistant, text.to_owned());
    assert_eq!(
        *asked.lock().unwrap(),
        [
            vec![user("my dad died")],
            vec![user("my dad died\n\nare you there?")],
            vec![
                user("my dad died\n\nare you there?"),
                nova("I'm here."),
                user("thanks")
            ],
        ]
    );
    // Only replies reach stdout, without the model's leading whitespace.
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Nova> I'm here.\nNova> Always.\n"
    );
    let notes = notes.text();
    assert!(notes.contains("no reply: "), "{notes}");
    assert!(notes.contains("Your message was kept"), "{notes}");
}

/// The doctor token ends the chat without reaching the model, and a frozen
/// companion refuses to chat at all.
#[test]
fn the_doctor_token_ends_the_chat_and_frozen_memory_refuses_one() {
    let dir = tempfile::tempdir().unwrap();
    let (model, asked) = scripted(vec![Ok("Hello.")]);
    let chat = open_with(dir.path(), model);
    let notes = Notes::default();
    converse(
        chat,
        open(dir.path()),
        4000,
        format!("hi\n{TOKEN}\nstill there?\n").as_bytes(),
        Vec::new(),
        notes.clone(),
    )
    .unwrap();
    assert_eq!(asked.lock().unwrap().len(), 1, "only \"hi\" was answered");
    assert!(notes.text().contains("Memory frozen for the doctor"));

    // With no reply scripted, a request would panic.
    let (model, asked) = scripted(Vec::new());
    let refused = converse(
        open_with(dir.path(), model),
        open(dir.path()),
        4000,
        "hi\n".as_bytes(),
        Vec::new(),
        Notes::default(),
    )
    .unwrap_err();
    assert!(refused.to_string().contains("frozen"), "{refused}");
    assert!(asked.lock().unwrap().is_empty());
}

/// When the terminal is gone the conversation is over: no panic, and no model
/// call for input that is still waiting.
#[test]
fn a_closed_terminal_ends_the_chat_without_a_model_call() {
    let dir = tempfile::tempdir().unwrap();
    let (model, asked) = scripted(Vec::new());
    converse(
        open_with(dir.path(), model),
        open(dir.path()),
        4000,
        "hi\n".as_bytes(),
        Vec::new(),
        Closed,
    )
    .unwrap();
    assert!(asked.lock().unwrap().is_empty());
}
