//! `clanka chat`: talk with a companion in the terminal. One model does both
//! jobs: it writes each reply, then reflects on the exchange in the background
//! as the writer. Lines in, replies out; `/sleep` ends the conversation.

use anyhow::{Result, bail};
use clanka_core::deps::{LlmMessage, LlmRequest, Role};
use clanka_core::model::Speaker;
use clanka_core::{Companion, Ranking};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Replies are in character, not bookkeeping, so warmer than the writer's 0.2.
const TEMPERATURE: f32 = 0.8;

pub fn run(mut chat: Companion, writer: Companion, budget: usize) -> Result<()> {
    if chat.is_frozen()? {
        bail!("memory is frozen by the doctor; unfreeze it before chatting");
    }
    let writer = Arc::new(Mutex::new(writer));
    let mut reflections: Vec<JoinHandle<()>> = Vec::new();
    let name = chat.soul().traits.name.clone();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        eprint!("you> ");
        std::io::stderr().flush()?;
        let Some(line) = lines.next() else { break };
        let message = line?;
        let message = message.trim();
        if message.is_empty() {
            continue;
        }
        if chat.soul().is_doctor_token(message) {
            chat.set_frozen(true)?;
            eprintln!("Memory frozen for the doctor. Nothing was recorded.");
            break;
        }
        if message == "/sleep" {
            finish(&mut reflections);
            let session = chat.open_session()?;
            let report = lock(&writer).sleep(session)?;
            eprintln!(
                "{name} is asleep: {} memories shifted, {} unresolved things weigh more.",
                report.drifted.len(),
                report.escalated.len()
            );
            break;
        }

        let context = chat.assemble(message, budget, &Ranking::default())?;
        let mut messages: Vec<LlmMessage> = context
            .turns
            .iter()
            .map(|t| LlmMessage {
                role: match t.speaker {
                    Speaker::User => Role::User,
                    Speaker::Companion => Role::Assistant,
                },
                content: t.content.clone(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            })
            .collect();
        messages.push(LlmMessage::user(message));
        let reply = chat.deps().llm.complete(&LlmRequest {
            system: context.render(),
            messages,
            tools: Vec::new(),
            temperature: TEMPERATURE,
        })?;
        let reply = reply.text.trim().to_owned();
        println!("{name}> {reply}");

        let session = chat.open_session()?;
        let user_turn = chat.record_turn(session, Speaker::User, message)?;
        let companion_turn = chat.record_turn(session, Speaker::Companion, &reply)?;
        let recalled: Vec<i64> = context.memories.iter().map(|r| r.memory.id).collect();
        chat.mark_recalled(session, &recalled)?;
        // Same model, separate call, off the hot path.
        let writer = writer.clone();
        reflections.push(std::thread::spawn(move || {
            if let Err(e) = lock(&writer).reflect(user_turn, companion_turn) {
                eprintln!("clanka: reflection on turns {user_turn}–{companion_turn} failed: {e}");
            }
        }));
        reflections.retain(|r| !r.is_finished());
    }
    finish(&mut reflections);
    Ok(())
}

/// Waits for reflections still running, so none is cut off at exit or sleep.
fn finish(reflections: &mut Vec<JoinHandle<()>>) {
    for r in reflections.drain(..) {
        let _ = r.join();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
