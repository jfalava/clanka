//! `clanka chat`: talk with a companion in the terminal. One model does both
//! jobs: it writes each reply, then reflects on the exchange in the background
//! as the writer. Lines in, replies out as they are written; `/sleep` ends the
//! conversation. Turns go through [`crate::turn`], like every other front end.
//!
//! A message starting with `~~` after sleep is a dream. Its replies may call
//! `wake_up`, so they arrive whole instead of as they are written; when the
//! companion wakes, the chat goes on awake. Leaving mid-dream drops it.

use crate::turn::{self, Opened};
use anyhow::{Result, bail};
use clanka_core::deps::{Llm, LlmMessage, LlmRequest, LlmResponse, Role};
use clanka_core::model::Speaker;
use clanka_core::tics::Dashless;
use clanka_core::{Companion, WakeReport, dream};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Replies are in character, not bookkeeping, so warmer than the writer's 0.2.
const TEMPERATURE: f32 = 0.8;

/// Said after a reply failed. Not "say it again": that would record it twice.
const KEPT: &str = "Your message was kept; go on and the next reply answers it too.";

pub fn run(chat: Companion, writer: Companion, budget: usize) -> Result<()> {
    converse(
        chat,
        writer,
        budget,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        std::io::stderr(),
    )
}

/// [`run`] on any streams: lines come from `input`, replies go to `out`, and
/// the prompt and clanka's own notes go to `notes`.
pub fn converse(
    mut chat: Companion,
    writer: Companion,
    budget: usize,
    input: impl BufRead,
    mut out: impl Write,
    notes: impl Write + Send + 'static,
) -> Result<()> {
    if chat.is_frozen()? {
        bail!("memory is frozen by the doctor; unfreeze it before chatting");
    }
    let notes = Notes(Arc::new(Mutex::new(Box::new(notes))));
    let writer = Arc::new(Mutex::new(writer));
    let mut reflections: Vec<JoinHandle<()>> = Vec::new();
    let name = chat.soul().traits.name.clone();
    let mut lines = input.lines();
    loop {
        // The terminal or pipe is gone: the conversation is over.
        if !notes.tell("you> ", false) {
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
            chat.drop_lost_dream()?;
            // In a dream, sleep is waking up.
            if let Some(dream) = chat.dreaming()? {
                wake(&writer, dream, &notes, &name);
                break;
            }
            let session = chat.open_session()?;
            let report = lock(&writer).sleep(session)?;
            notes.say(&format!(
                "{name} is asleep: {} memories shifted, {} unresolved things weigh more.",
                report.drifted.len(),
                report.escalated.len()
            ));
            break;
        }

        let mut open = match turn::open(&mut chat, message, budget, true)? {
            Opened::Doctor => {
                notes.say("Memory frozen for the doctor. Nothing was recorded.");
                break;
            }
            Opened::Frozen => bail!("memory is frozen by the doctor; unfreeze it before chatting"),
            Opened::Awake => {
                notes.say(&format!(
                    "clanka: {name} is awake in this conversation. Send /sleep, then start \
                     the dream with ~~ in a new chat."
                ));
                continue;
            }
            Opened::Open(open) => *open,
        };
        let mut system = open.context.render();
        for hint in open.hints() {
            system = format!("{system}\n\n{hint}");
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
        let mut request = LlmRequest {
            system,
            messages,
            tools: if open.pending.dream {
                vec![dream::wake_up_tool()]
            } else {
                Vec::new()
            },
            temperature: TEMPERATURE,
        };
        let llm = chat.deps().llm.as_ref();
        let mut reply = speak(llm, &request, &name, &mut out);
        // wake_up is answered before the reply goes on: what the model wrote
        // before the call is still the dream, and what it writes after the
        // answer is the moment it wakes, or more of the dream.
        let mut before = String::new();
        if let Ok(called) = &reply
            && let Some(because) = turn::wake_call(&called.tool_calls)
        {
            let (_, answer) = turn::wake_up(&chat, &mut open.pending, &because)?;
            before = called.text.trim().to_owned();
            request.messages.push(LlmMessage {
                role: Role::Assistant,
                content: called.text.clone(),
                tool_calls: called.tool_calls.clone(),
                tool_call_id: None,
            });
            for call in &called.tool_calls {
                request.messages.push(LlmMessage {
                    role: Role::Tool,
                    content: answer.into(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(call.id.clone()),
                });
            }
            request.tools.clear();
            reply = speak(llm, &request, &name, &mut out);
        }
        let reply = reply.map(|r| {
            [before.as_str(), r.text.trim()]
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        });
        let reply = match reply {
            Ok(reply) if !reply.is_empty() => reply,
            // The message is already recorded: the next reply answers both,
            // and the reflection on that exchange reads both.
            Ok(_) => {
                notes.say(&format!("clanka: the model sent an empty reply. {KEPT}"));
                continue;
            }
            Err(e) => {
                notes.say(&format!("clanka: no reply: {e}\nclanka: {KEPT}"));
                continue;
            }
        };
        let (dreaming, session, wakes) =
            (open.pending.dream, open.pending.session, open.pending.wake);
        let (user_turn, companion_turn) = turn::end(&mut chat, open.pending, &reply)?;
        // Nothing of a dream is kept as it happens: it is decided on waking.
        if dreaming {
            if wakes {
                finish(&mut reflections);
                wake(&writer, session, &notes, &name);
            }
            continue;
        }
        // Same model, separate call, off the hot path.
        let (writer, notes) = (writer.clone(), notes.clone());
        reflections.push(std::thread::spawn(move || {
            if let Err(e) = lock(&writer).reflect(user_turn, companion_turn) {
                notes.say(&format!(
                    "clanka: reflection on turns {user_turn}–{companion_turn} failed: {e}"
                ));
            }
        }));
        reflections.retain(|r| !r.is_finished());
    }
    finish(&mut reflections);
    // A dream that didn't end by waking is dropped: nothing of it is kept.
    if chat.drop_dream()?.is_some() {
        notes.say(&format!(
            "clanka: the dream ended before {name} woke; nothing of it was kept."
        ));
    }
    Ok(())
}

/// Sends `request` and prints the reply as `name> …`, as the model writes it
/// when it can stream (a reply that may call a tool arrives whole), without
/// its em dashes: what is shown is what [`turn::end`] records.
fn speak(
    llm: &dyn Llm,
    request: &LlmRequest,
    name: &str,
    out: &mut impl Write,
) -> clanka_core::Result<LlmResponse> {
    let mut started = false;
    let mut dashless = Dashless::default();
    let reply = llm.stream(request, &mut |piece| {
        let piece = dashless.push(piece);
        let piece = if started {
            piece.as_str()
        } else {
            piece.trim_start()
        };
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
        let _ = writeln!(out, "{}", dashless.finish().trim_end());
    }
    reply
}

/// Wakes the companion from `dream` and says what stayed. A wake that fails
/// leaves the dream open: /sleep tries again, and leaving drops it.
fn wake(writer: &Mutex<Companion>, dream: i64, notes: &Notes, name: &str) {
    match lock(writer).wake(dream) {
        Ok(report) => notes.say(&woke(name, &report)),
        Err(e) => notes.say(&format!(
            "clanka: {name} couldn't wake: {e}. Send /sleep to try again."
        )),
    }
}

/// What waking left, in a line.
fn woke(name: &str, report: &WakeReport) -> String {
    let seared = if report.seared.is_some() {
        "; one thing an echo showed stays, burned in"
    } else {
        ""
    };
    format!(
        "{name} wakes: {} fragments linger, {} memories shifted{seared}.",
        report.fragments.len(),
        report.drifted.len()
    )
}

/// Where the prompt and clanka's notes go: stderr in a terminal. Shared with
/// the reflections, which report their failures from their own threads.
#[derive(Clone)]
struct Notes(Arc<Mutex<Box<dyn Write + Send>>>);

impl Notes {
    /// Writes the text, and says whether anyone is still there to read it.
    /// `eprintln!` panics on a closed terminal or pipe, and a panic here would
    /// end the process under a reflection that is still writing.
    fn tell(&self, text: &str, line: bool) -> bool {
        let mut notes = lock(&self.0);
        let end = if line { "\n" } else { "" };
        write!(notes, "{text}{end}")
            .and_then(|()| notes.flush())
            .is_ok()
    }

    fn say(&self, text: &str) {
        self.tell(text, true);
    }
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
