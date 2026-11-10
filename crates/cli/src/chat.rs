//! `clanka chat`: talk with a companion in the terminal. One model does both
//! jobs: it writes each reply, then reflects on the exchange in the background
//! as the writer. Lines in, replies out as they are written; `/sleep` ends the
//! conversation. Turns go through [`crate::turn`], like every other front end.

use crate::turn::{self, Opened};
use anyhow::{Result, bail};
use clanka_core::Companion;
use clanka_core::deps::{LlmMessage, LlmRequest, Role};
use clanka_core::model::Speaker;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Replies are in character, not bookkeeping, so warmer than the writer's 0.2.
const TEMPERATURE: f32 = 0.8;

/// Said after a reply failed. Not "say it again": that would record it twice.
const KEPT: &str = "Your message was kept; go on and the next reply answers it too.";

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
        // The terminal or pipe is gone: the conversation is over.
        if !tell("you> ", false) {
            break;
        }
        let Some(line) = lines.next() else { break };
        let message = line?;
        let message = message.trim();
        if message.is_empty() {
            continue;
        }
        if message == "/sleep" {
            finish(&mut reflections);
            let session = chat.open_session()?;
            let report = lock(&writer).sleep(session)?;
            say(&format!(
                "{name} is asleep: {} memories shifted, {} unresolved things weigh more.",
                report.drifted.len(),
                report.escalated.len()
            ));
            break;
        }

        let open = match turn::open(&mut chat, message, budget, true)? {
            Opened::Doctor => {
                say("Memory frozen for the doctor. Nothing was recorded.");
                break;
            }
            Opened::Frozen => bail!("memory is frozen by the doctor; unfreeze it before chatting"),
            Opened::Open(open) => *open,
        };
        let mut system = open.context.render();
        if let Some(n) = open.stretch {
            system = format!("{system}\n\n{}", turn::stretch_hint(n));
        }
        let said = open
            .context
            .turns
            .iter()
            .map(|t| {
                let role = match t.speaker {
                    Speaker::User => Role::User,
                    Speaker::Companion => Role::Assistant,
                };
                (role, t.content.as_str())
            })
            .chain([(Role::User, open.message.as_str())]);
        let mut messages: Vec<LlmMessage> = Vec::new();
        for (role, content) in said {
            match messages.last_mut() {
                // A message whose reply failed is on record with no reply after
                // it. It goes out joined to the next: many providers refuse
                // two user messages in a row.
                Some(last) if last.role == Role::User && role == Role::User => {
                    last.content.push_str("\n\n");
                    last.content.push_str(content);
                }
                _ => messages.push(LlmMessage {
                    role,
                    content: content.to_owned(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                }),
            }
        }
        let request = LlmRequest {
            system,
            messages,
            tools: Vec::new(),
            temperature: TEMPERATURE,
        };
        // The reply is printed as the model writes it.
        let mut out = std::io::stdout().lock();
        let mut started = false;
        let reply = chat.deps().llm.stream(&request, &mut |piece| {
            let piece = if started { piece } else { piece.trim_start() };
            if piece.is_empty() {
                return;
            }
            if !started {
                let _ = write!(out, "{name}> ");
                started = true;
            }
            let _ = write!(out, "{piece}");
            let _ = out.flush();
        });
        if started {
            let _ = writeln!(out);
        }
        drop(out);
        let reply = match reply {
            Ok(reply) if !reply.text.trim().is_empty() => reply.text.trim().to_owned(),
            // The message is already recorded: the next reply answers both,
            // and the reflection on that exchange reads both.
            Ok(_) => {
                say(&format!("clanka: the model sent an empty reply. {KEPT}"));
                continue;
            }
            Err(e) => {
                say(&format!("clanka: no reply: {e}\nclanka: {KEPT}"));
                continue;
            }
        };
        let (user_turn, companion_turn) = turn::end(&mut chat, open.pending, &reply)?;
        // Same model, separate call, off the hot path.
        let writer = writer.clone();
        reflections.push(std::thread::spawn(move || {
            if let Err(e) = lock(&writer).reflect(user_turn, companion_turn) {
                say(&format!(
                    "clanka: reflection on turns {user_turn}–{companion_turn} failed: {e}"
                ));
            }
        }));
        reflections.retain(|r| !r.is_finished());
    }
    finish(&mut reflections);
    Ok(())
}

/// Writes to stderr, and says whether anyone is still there to read it.
/// `eprintln!` panics on a closed terminal or pipe, and a panic here would end
/// the process under a reflection that is still writing.
fn tell(text: &str, line: bool) -> bool {
    let mut err = std::io::stderr().lock();
    let end = if line { "\n" } else { "" };
    write!(err, "{text}{end}")
        .and_then(|()| err.flush())
        .is_ok()
}

fn say(text: &str) {
    tell(text, true);
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
