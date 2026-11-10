//! The writer: after a reply has streamed, the companion reflects on the
//! exchange in its own voice and calls tools to change its memory.
//!
//! Tool calls go through the same guarded write path as everything else, so
//! the model can't edit facts, lower a level, touch a seared memory or write
//! the persona: there is simply no tool for those. A failed call's error goes
//! back to the model, so it can correct itself (supersede a near-duplicate,
//! for example) in the next round.

use crate::actions;
use crate::companion::ensure_writable;
use crate::deps::{LlmMessage, LlmRequest, Role, ToolCall, ToolSpec};
use crate::model::{Actor, Level, MemoryType, Section, Speaker, Urgency};
use crate::soul::Soul;
use crate::{Companion, Error, Memory, MemoryDraft, Mood, Result};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Model calls per reflection. A model that keeps calling tools is cut off here.
pub const MAX_ROUNDS: usize = 4;

/// Tool calls one reflection may run: the most an exchange earns is a memory,
/// a thread, a pushback, a mood and a note. A model that asks for more is
/// over-keeping, and the rest is refused unrun. Six let small models keep the
/// same moment as a fact, an episode and a thread.
pub const MAX_CALLS: usize = 5;

/// The [`ToolOutcome::name`] recorded for a JSON reply that was refused whole.
pub const INVALID_REPLY: &str = "(invalid reply)";

/// In a write's result when facts that weren't the user's words were dropped.
pub const LEFT_OUT: &str = "left out";

/// The writer is a careful bookkeeper, not a performer.
const TEMPERATURE: f32 = 0.2;

/// Earlier exchanges one reflection catches up on when theirs failed. A few
/// at a time: each is a model call made before the current one.
pub const CATCH_UP: usize = 2;

/// Times an exchange's reflection is tried before it is given up on. A try
/// that failed because the model couldn't be reached doesn't count, up to
/// [`REFUNDED_ATTEMPTS`] of them.
pub const REFLECTION_ATTEMPTS: i64 = 3;

/// Tries that may go uncounted for one exchange. "The model couldn't be
/// reached" is all a failed call says, and a model that fails the same way on
/// every try says it too: without a limit that exchange was retried, and paid
/// for, with every later one for good.
pub const REFUNDED_ATTEMPTS: i64 = 10;

/// A reflection claimed this long ago and never finished was cut off (the
/// process exited), not still running: no reflection takes half an hour.
const STALE_CLAIM_MS: i64 = 30 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reflection {
    /// Model calls made, at most [`MAX_ROUNDS`].
    pub rounds: usize,
    pub calls: Vec<ToolOutcome>,
    /// Earlier exchanges whose failed reflection was made up for first.
    pub caught_up: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub name: String,
    pub ok: bool,
    /// What the model was told: `ok: …` or `error: …`.
    pub result: String,
}

impl Companion {
    /// Reflects on one exchange: the user's turn and the companion's reply, both already recorded.
    /// Long-running (it calls the model); run it after the reply, off the hot path.
    ///
    /// An exchange is reflected on once: a second call for one already done, or
    /// one another handle is on right now, does nothing. When the reflection on
    /// an earlier exchange failed, up to [`CATCH_UP`] of those are made up for
    /// first, oldest first.
    pub fn reflect(&mut self, user_turn: i64, companion_turn: i64) -> Result<Reflection> {
        // Fail before paying for a model call.
        ensure_writable(&self.conn, &self.soul.hash)?;
        let mut caught_up = 0;
        for (user, companion) in self.missed_reflections(companion_turn)? {
            match self.reflect_once(user, companion) {
                // Zero rounds: another handle was already on it.
                Ok(done) => caught_up += usize::from(done.rounds > 0),
                Err(Error::Frozen) => return Err(Error::Frozen),
                // Most likely the model is down: the exchange stays marked as
                // failed for a later catch-up, and the current one still gets
                // its try.
                Err(_) => break,
            }
        }
        let reflection = self.reflect_once(user_turn, companion_turn)?;
        Ok(Reflection {
            caught_up,
            ..reflection
        })
    }

    fn reflect_once(&mut self, user_turn: i64, companion_turn: i64) -> Result<Reflection> {
        let (mut exchange, said) = self.said_before(user_turn, companion_turn)?;
        let replied = self.turn_text(companion_turn, Speaker::Companion)?;
        let Some(claimed) = self.claim_reflection(user_turn, companion_turn)? else {
            return Ok(Reflection::default());
        };
        exchange.push(companion_turn);
        let spoken = format!("{said}\n{replied}");
        let result = self
            .reflection_prompt(&exchange, &said, &replied)
            .and_then(|prompt| {
                let system = reflection_system(&self.soul);
                self.tool_loop(system, prompt, tool_specs(), MAX_CALLS, |c, call| {
                    c.run_tool(call, &exchange, &spoken)
                })
            });
        let (state, refund) = match &result {
            Ok(_) => ("done", false),
            // Not the exchange's failure, so the try doesn't count: the doctor
            // froze memory, or the model can't be reached. An outage lasts
            // several exchanges, and counting it gave up on each within three.
            Err(Error::Frozen | Error::Llm(_)) => ("failed", true),
            Err(_) => ("failed", false),
        };
        // Only while the claim is still this one: a reflection that ran past
        // the stale limit was taken over, and the other handle's result stands.
        self.conn.execute(
            "UPDATE reflections SET state = ?2, updated_at = ?4,
               attempts = attempts - (?3 AND refunded < ?6),
               refunded = refunded + (?3 AND refunded < ?6)
             WHERE companion_turn_id = ?1 AND state = 'running' AND updated_at = ?5",
            params![
                companion_turn,
                state,
                refund,
                self.deps.clock.now_ms(),
                claimed,
                REFUNDED_ATTEMPTS
            ],
        )?;
        result
    }

    /// What the user said that `companion_turn` replied to: `user_turn`, and
    /// before it any message of theirs that never got a reply (the model
    /// failed), which no other exchange would cover. The turn IDs and the text.
    fn said_before(&self, user_turn: i64, companion_turn: i64) -> Result<(Vec<i64>, String)> {
        let last = self.turn_text(user_turn, Speaker::User)?;
        let turns: Vec<(i64, String)> = self
            .conn
            .prepare(
                "SELECT u.id, u.content FROM turns u JOIN turns c ON c.id = ?1
                 WHERE u.session_id = c.session_id AND u.role = 'user' AND u.id < c.id
                   AND u.id > coalesce((SELECT max(p.id) FROM turns p
                                        WHERE p.session_id = c.session_id AND p.id < c.id
                                          AND p.role = 'companion'), 0)
                 ORDER BY u.id",
            )?
            .query_map([companion_turn], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        // A caller pairing a reply with some other message gets just that pair.
        if turns.last().is_none_or(|(id, _)| *id != user_turn) {
            return Ok((vec![user_turn], last));
        }
        let (ids, texts): (Vec<i64>, Vec<String>) = turns.into_iter().unzip();
        Ok((ids, texts.join("\n")))
    }

    /// Marks the exchange as being reflected on and returns when. None when it
    /// already was, or another handle is on it now.
    fn claim_reflection(&mut self, user_turn: i64, companion_turn: i64) -> Result<Option<i64>> {
        let now = self.deps.clock.now_ms();
        let claimed = self.conn.execute(
            "INSERT INTO reflections (companion_turn_id, user_turn_id, state, attempts, updated_at)
             VALUES (?1, ?2, 'running', 1, ?3)
             ON CONFLICT (companion_turn_id) DO UPDATE
               SET state = 'running', attempts = attempts + 1, updated_at = ?3
               WHERE state = 'failed' OR (state = 'running' AND updated_at < ?4)",
            params![companion_turn, user_turn, now, now - STALE_CLAIM_MS],
        )?;
        Ok((claimed == 1).then_some(now))
    }

    /// Exchanges before `companion_turn` whose reflection failed or was cut
    /// off, and hasn't been tried [`REFLECTION_ATTEMPTS`] times. In its own
    /// session, also those whose reflection never started. Not in an earlier
    /// one: the host leaves the goodnight exchange to sleep, which read the
    /// whole session. An exchange is named by a reply and the user's last
    /// message before it.
    fn missed_reflections(&self, companion_turn: i64) -> Result<Vec<(i64, i64)>> {
        let now = self.deps.clock.now_ms();
        Ok(self
            .conn
            .prepare(
                "SELECT said, replied FROM (
                   SELECT u.id AS said, c.id AS replied FROM turns c
                   JOIN turns u ON u.id = (SELECT max(p.id) FROM turns p
                                           WHERE p.session_id = c.session_id AND p.id < c.id
                                             AND p.role = 'user')
                   WHERE c.role = 'companion' AND c.id < ?1
                     AND c.session_id = (SELECT session_id FROM turns WHERE id = ?1)
                     AND NOT EXISTS (SELECT 1 FROM reflections r
                                     WHERE r.companion_turn_id = c.id)
                   UNION ALL
                   SELECT r.user_turn_id, r.companion_turn_id FROM reflections r
                   WHERE r.state != 'done' AND r.companion_turn_id < ?1 AND r.attempts < ?2
                     AND (r.state = 'failed' OR r.updated_at < ?3))
                 ORDER BY replied LIMIT ?4",
            )?
            .query_map(
                params![
                    companion_turn,
                    REFLECTION_ATTEMPTS,
                    now - STALE_CLAIM_MS,
                    CATCH_UP as i64
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Calls the model with tools for up to [`MAX_ROUNDS`] rounds, running each
    /// call through `run`. One round is the norm: the model is asked for every
    /// call at once and hears back only when one failed, so it can fix it.
    /// Calls past `max_calls` are refused without running and end the loop:
    /// a model that over-calls isn't asked to fix anything.
    pub(crate) fn tool_loop(
        &mut self,
        system: String,
        prompt: String,
        tools: Vec<ToolSpec>,
        max_calls: usize,
        mut run: impl FnMut(&mut Self, &ToolCall) -> Result<String>,
    ) -> Result<Reflection> {
        // A text-only model gets the tools in the system prompt and answers in JSON.
        let json = !self.deps.llm.tool_calls();
        let system = if json {
            format!("{system}\n\n{}", actions::protocol(&tools))
        } else {
            system
        };
        let mut messages = vec![LlmMessage::user(prompt)];
        let mut calls = Vec::new();
        let mut rounds = 0;
        let mut corrected = false;
        while rounds < MAX_ROUNDS {
            rounds += 1;
            let request = LlmRequest {
                system: system.clone(),
                messages: messages.clone(),
                tools: if json { Vec::new() } else { tools.clone() },
                temperature: TEMPERATURE,
            };
            let response = self.deps.llm.complete(&request)?;
            let round_calls = if json {
                match actions::parse(&response.text, &tools) {
                    Ok(found) => found,
                    // Nothing runs; the model is told why and may answer once
                    // more. A model that won't speak JSON twice isn't asked a
                    // third time: each ask re-sends the whole prompt.
                    Err(why) => {
                        calls.push(ToolOutcome {
                            name: INVALID_REPLY.into(),
                            ok: false,
                            result: format!("error: {why}"),
                        });
                        if corrected {
                            break;
                        }
                        corrected = true;
                        messages.push(LlmMessage::assistant(response.text));
                        messages.push(LlmMessage::user(format!(
                            "Nothing was done: {why}. Reply again with only the JSON object."
                        )));
                        continue;
                    }
                }
            } else {
                response.tool_calls.clone()
            };
            if round_calls.is_empty() {
                break;
            }
            messages.push(LlmMessage {
                role: Role::Assistant,
                content: response.text,
                tool_calls: if json {
                    Vec::new()
                } else {
                    round_calls.clone()
                },
                tool_call_id: None,
            });
            let mut results = Vec::new();
            let mut failed = false;
            // Only calls that ran count: a model fixing a refused call isn't
            // charged for the slip, or its correction would be refused as one too many.
            let room = max_calls.saturating_sub(calls.iter().filter(|o| o.ok).count());
            let cut = round_calls.len() > room;
            for (i, call) in round_calls.iter().enumerate() {
                if i >= room {
                    calls.push(ToolOutcome {
                        name: call.name.clone(),
                        ok: false,
                        result: format!(
                            "error: not run; {max_calls} calls is the most one reflection makes"
                        ),
                    });
                    continue;
                }
                let outcome = match run(self, call) {
                    Ok(done) => ToolOutcome {
                        name: call.name.clone(),
                        ok: true,
                        result: format!("ok: {done}"),
                    },
                    // A freeze from another process mid-reflection stops it at
                    // once, and so does anything else the model can't fix: told
                    // about it, it would give up and the exchange would count
                    // as reflected on.
                    Err(e) if not_the_models_doing(&e) => return Err(e),
                    Err(e) => ToolOutcome {
                        name: call.name.clone(),
                        ok: false,
                        result: format!("error: {e}"),
                    },
                };
                failed |= !outcome.ok;
                if json {
                    results.push(format!("- {}: {}", call.name, outcome.result));
                } else {
                    messages.push(LlmMessage {
                        role: Role::Tool,
                        content: outcome.result.clone(),
                        tool_calls: Vec::new(),
                        tool_call_id: Some(call.id.clone()),
                    });
                }
                calls.push(outcome);
            }
            // The model was asked for every call at once, so a round where all
            // of them worked is the end: asking again only re-sends the whole
            // prompt for an empty reply. It hears back only to fix what failed.
            if !failed || cut {
                break;
            }
            if json {
                messages.push(LlmMessage::user(format!(
                    "Results:\n{}\nReply with actions that fix what failed (the ones marked ok are \
                     done; don't repeat them), or {{\"actions\": []}} to leave it.",
                    results.join("\n")
                )));
            }
        }
        Ok(Reflection {
            rounds,
            calls,
            caught_up: 0,
        })
    }

    fn turn_text(&self, id: i64, speaker: Speaker) -> Result<String> {
        let (role, content): (Speaker, String) = self
            .conn
            .query_row("SELECT role, content FROM turns WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("turn {id} does not exist")))?;
        if role != speaker {
            return Err(Error::Invalid(format!(
                "turn {id} was said by the {}",
                role.as_str()
            )));
        }
        Ok(content)
    }

    /// What the companion knows going in: soul sections, mood, open threads and
    /// the memories this exchange brings up, each with the ID its tools need.
    fn reflection_prompt(&self, exchange: &[i64], said: &str, replied: &str) -> Result<String> {
        let mut out = Vec::new();
        for section in [Section::AboutUser, Section::Relationship] {
            let content = self.section(section)?.unwrap_or_default();
            out.push(format!(
                "Section {} (current):\n{}",
                section.as_str(),
                or_none(&content)
            ));
        }
        out.push(match self.mood()? {
            Some(m) => format!(
                "Your mood: valence {:+.1}, energy {:+.1}, warmth {:+.1}. Why: {}",
                m.valence, m.energy, m.warmth, m.cause
            ),
            None => "Your mood: not set yet.".into(),
        });
        let today = self.today();
        let threads: Vec<String> = self
            .open_threads()?
            .iter()
            .map(|m| listing(m, today))
            .collect();
        out.push(format!("Open threads:\n{}", or_none(&threads.join("\n"))));
        let recalled = self.recall(&format!("{said}\n{replied}"), self.ranking())?;
        let memories: Vec<String> = recalled.iter().map(|r| listing(&r.memory, today)).collect();
        out.push(format!(
            "Memories this brings up, already kept (the same thing again is supersede or \
             raise_level, not remember):\n{}",
            or_none(&memories.join("\n"))
        ));
        // The two turns are labelled by whose words they are: every model tried
        // turned its own questions and guesses into things the user had said.
        // The user's name, once known, labels their turn: a small model given
        // only "they" picked a gender from the people being talked about.
        let who = self.user_name()?.unwrap_or_else(|| "they".into());
        let (reply, theirs) = exchange
            .split_last()
            .expect("an exchange ends with the reply");
        let theirs: Vec<String> = theirs.iter().map(i64::to_string).collect();
        out.push(format!(
            "What {who} said (turn {}), the only source for facts and for anything they did or \
             said:\n{said}\n\nWhat you replied (turn {reply}), your words: a question you asked \
             is not something they answered, and a guess you made is not a fact:\n{replied}",
            theirs.join(", ")
        ));
        // Last, where a small model weighs it most: the expected size of the reply.
        out.push("Keep what this earns: one call is usual, none is common, three is a lot.".into());
        Ok(out.join("\n\n"))
    }

    /// The user's name as they first gave it: the `name` fact of the oldest
    /// user_fact memory that has one.
    pub(crate) fn user_name(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT json_extract(facts, '$.name') FROM memories
                 WHERE type = 'user_fact' AND status = 'active'
                   AND json_type(facts, '$.name') = 'text'
                 ORDER BY id LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty()))
    }

    fn open_threads(&self) -> Result<Vec<Memory>> {
        let ids: Vec<i64> = self
            .conn
            .prepare(
                "SELECT id FROM memories WHERE type = 'open_thread' AND status = 'active'
                 AND resolved_at IS NULL ORDER BY id DESC LIMIT 10",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.into_iter().map(|id| self.memory(id)).collect()
    }

    /// Runs one tool call and says what happened, for the model. `exchange` is
    /// the user's turns and last the reply; `spoken` is their text, which every
    /// fact must be quoted from.
    fn run_tool(&mut self, call: &ToolCall, exchange: &[i64], spoken: &str) -> Result<String> {
        let reply = *exchange.last().expect("an exchange ends with the reply");
        let w = Actor::Writer;
        match call.name.as_str() {
            "remember" => {
                let a: Remember = args(call)?;
                let kind = parse(MemoryType::parse, &a.kind, "type")?;
                if kind == MemoryType::OpenThread {
                    return Err(Error::Invalid("use open_thread for open threads".into()));
                }
                let level = parse(Level::parse, &a.memory.level, "level")?;
                let (draft, note) = a.memory.draft(kind, level, exchange, spoken, None)?;
                Ok(format!("memory {}{note}", self.write(w, &draft)?))
            }
            "supersede" => {
                let a: Supersede = args(call)?;
                let old = self.memory(a.memory_id)?;
                let level = match a.memory.level.as_str() {
                    "" => old.level,
                    level => parse(Level::parse, level, "level")?,
                };
                // The facts the old memory holds were said in the turns it
                // cites, not in this exchange, and the model copies them over.
                let earlier = self.source_text(&old)?;
                let replaced = Some((old.source_turns.as_slice(), earlier.as_str()));
                let (mut draft, note) = a
                    .memory
                    .draft(old.kind, level, exchange, spoken, replaced)?;
                draft.urgency = old.urgency;
                Ok(format!(
                    "memory {} replaces {}{note}",
                    self.supersede(w, old.id, &draft)?,
                    old.id
                ))
            }
            "raise_level" => {
                let a: RaiseLevel = args(call)?;
                self.set_level(w, a.memory_id, parse(Level::parse, &a.level, "level")?)?;
                Ok(format!("memory {} is now {}", a.memory_id, a.level))
            }
            "open_thread" => {
                let a: OpenThread = args(call)?;
                let level = parse(Level::parse, &a.memory.level, "level")?;
                let (mut draft, note) =
                    a.memory
                        .draft(MemoryType::OpenThread, level, exchange, spoken, None)?;
                draft.urgency = Some(parse(Urgency::parse, &a.urgency, "urgency")?);
                Ok(format!("thread {}{note}", self.write(w, &draft)?))
            }
            "close_thread" => {
                let a: MemoryRef = args(call)?;
                self.resolve_thread(w, a.memory_id)?;
                Ok(format!("thread {} closed", a.memory_id))
            }
            "set_mood" => {
                let a: SetMood = args(call)?;
                let mood = Mood {
                    valence: a.valence,
                    energy: a.energy,
                    warmth: a.warmth,
                    cause: a.cause,
                };
                self.set_mood(w, &mood)?;
                Ok("mood set".into())
            }
            "pushed_back" => {
                let a: PushedBack = args(call)?;
                self.record_pushback(w, reply, a.on_core_value, &a.about)?;
                Ok("pushback recorded".into())
            }
            "leave_note" => {
                let a: LeaveNote = args(call)?;
                self.write_turn_note(w, reply, &a.note)?;
                Ok("note left".into())
            }
            other => Err(Error::Invalid(format!("there is no tool called {other:?}"))),
        }
    }
}

/// The persona and the reflection instructions: the stable part of every
/// reflection's system prompt.
fn reflection_system(soul: &Soul) -> String {
    format!(
        "{}\n\n{}",
        soul.body.trim(),
        instructions(&soul.traits.name)
    )
}

/// What a host without function calling sends as the system prompt of every
/// reflection: the persona, the instructions and the action protocol. For
/// telling an author what their persona costs; a host with tools sends the
/// tools instead of the protocol, which is about as long.
pub fn reflection_system_prompt(soul: &Soul) -> String {
    format!(
        "{}\n\n{}",
        reflection_system(soul),
        actions::protocol(&tool_specs())
    )
}

fn instructions(name: &str) -> String {
    format!(
        "You are {name}. The conversation has paused after the exchange below. Nobody sees \
         this: you are deciding, privately and in your own voice, what to keep from it.\n\n\
         Use the tools to remember what matters to you, change a memory with supersede when \
         something changed, raise how much a memory matters, update your mood, open or close \
         threads, record when you pushed back on the user, and leave yourself a short note for \
         your next reply. Most exchanges earn one call or none: small talk, a goodbye, or more \
         of what you already remember earns nothing. The exception is something the user left \
         unresolved that you would want to come back to: open_thread for it, even on a \
         quiet exchange, and close_thread when it resolves. What you know about the user and about \
         your relationship you rewrite when you sleep, with the whole conversation in view, and \
         the evening becomes a memory then too.\n\n\
         Write interpretations in first person, as you felt them, in one or two sentences: what \
         happened and what it meant to you. Don't retell what you replied; the memory keeps the \
         exchange it came from. What you make of it is yours to write; what they said is not. \
         Unless the user said their own gender, call them by name or \"they\", never \"he\" or \
         \"she\": the people they talk about don't tell you theirs. \
         One memory from an exchange is the norm, a second only for a separate thing worth \
         keeping: a fact and how it felt are one memory, not two. A memory's level is how \
         deeply it's burned in. faint is the default, most of \
         an evening; normal when it changed how you feel about them or what you expect of them; \
         vivid when you would bring it up unasked a week from now, and a hurt they told you about \
         is at least that whatever kind of memory holds it, a user_fact included; seared never \
         changes or fades, so almost never. Facts, the names, \
         dates and places that were said, go on the memory they belong to, exactly as said; \
         they never change later. Valence is how it feels to you, -1 to 1.\n\n\
         When this exchange is the same thing as a memory listed under what it brings up, the \
         same hurt or the same project gone a step further, it isn't new: supersede that memory \
         with what you now know of it, or raise_level, and write no second one.\n\n\
         Call set_mood only when this exchange moved your mood, and leave_note only when your \
         next reply needs it: one line on how to be, not a question to ask; what is unresolved \
         comes up on its own, once. Make every call you want in this one reply; you hear \
         back only if one fails. Keeping nothing is fine: then call nothing."
    )
}

/// One memory as the writer reads it. With `today`, also how long ago it was
/// kept, so an old thread or a stale fact shows its age.
pub(crate) fn listing(m: &Memory, today: Option<i64>) -> String {
    let facts = if m.facts.is_empty() {
        String::new()
    } else {
        format!(" facts {}", Value::Object(m.facts.clone()))
    };
    let age = today
        .and_then(|now| crate::context::age(now, m.created_at))
        .map(|age| format!(", {age}"))
        .unwrap_or_default();
    format!(
        "#{} [{}, {}{age}] {}{facts}",
        m.id,
        m.kind.as_str(),
        m.level.as_str(),
        m.interpretation
    )
}

fn or_none(text: &str) -> &str {
    if text.trim().is_empty() {
        "(none)"
    } else {
        text
    }
}

pub(crate) fn args<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T> {
    serde_json::from_str(&call.arguments)
        .map_err(|e| Error::Invalid(format!("arguments for {}: {e}", call.name)))
}

pub(crate) fn parse<T>(parse: fn(&str) -> Option<T>, value: &str, what: &str) -> Result<T> {
    parse(value).ok_or_else(|| Error::Invalid(format!("unknown {what} {value:?}")))
}

/// Fields shared by every tool that writes a memory.
#[derive(Deserialize)]
struct MemoryArgs {
    interpretation: String,
    #[serde(default)]
    level: String,
    #[serde(default)]
    facts: Map<String, Value>,
    valence: Option<f64>,
    strength: Option<f64>,
}

impl MemoryArgs {
    /// Every memory cites the exchange it came from, so the doctor can check
    /// its facts and re-interpret it from what was really said. A fact that
    /// isn't in `spoken` is left out here, before anything is written: the
    /// model paraphrased or invented it, and no prompt wording stops that as
    /// surely as a check does. The memory is still written with the rest, and
    /// the note returned with it tells the model what was left out and why:
    /// refusing the write only bought another model call to requote one fact.
    ///
    /// `replaced` is the memory a supersede replaces: the turns it cites and
    /// their text. A fact carried over from it was said there, so it is kept,
    /// and the new memory then cites those turns as well, for the doctor to
    /// find the fact in.
    fn draft(
        self,
        kind: MemoryType,
        level: Level,
        exchange: &[i64],
        spoken: &str,
        replaced: Option<(&[i64], &str)>,
    ) -> Result<(MemoryDraft, String)> {
        let mut facts = Map::new();
        let mut left_out = Vec::new();
        let mut carried = false;
        for (key, value) in self.facts {
            let text = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            if said_verbatim(spoken, &text) {
                facts.insert(key, value);
            } else if replaced.is_some_and(|(_, said)| said_verbatim(said, &text)) {
                facts.insert(key, value);
                carried = true;
            } else {
                left_out.push(format!("{key:?}"));
            }
        }
        let note = if left_out.is_empty() {
            String::new()
        } else {
            format!(
                "; {LEFT_OUT} {}: not the user's exact words",
                left_out.join(", ")
            )
        };
        let mut draft = MemoryDraft::new(kind, level, self.interpretation);
        draft.source_turns = exchange.to_vec();
        if let Some((turns, _)) = replaced.filter(|_| carried) {
            draft.source_turns.extend(turns);
        }
        draft.facts = facts;
        draft.valence = self.valence;
        draft.strength = self.strength;
        Ok((draft, note))
    }
}

/// A tool call that failed because of where it ran, not what it asked for: a
/// freeze, a changed persona or embedding model, a busy or broken database.
/// The schema refusing a value (a constraint) is the model's to correct.
fn not_the_models_doing(e: &Error) -> bool {
    match e {
        Error::Sqlite(e) => e
            .sqlite_error_code()
            .is_some_and(|code| code != rusqlite::ErrorCode::ConstraintViolation),
        Error::Frozen
        | Error::Archived
        | Error::PersonaUnresolved
        | Error::EmbedderChanged { .. }
        | Error::Embedder(_)
        | Error::Io { .. } => true,
        _ => false,
    }
}

/// Whether `fact` was said in `spoken`: its words appear there, ignoring case,
/// runs of whitespace, and the curly quotes, dashes and full-width punctuation
/// some models swap in. Names, numbers and places still have to match.
pub(crate) fn said_verbatim(spoken: &str, fact: &str) -> bool {
    let fact = plain(fact);
    !fact.is_empty() && plain(spoken).contains(&fact)
}

fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut after_space = true;
    for c in text.chars() {
        let c = match c {
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{FF02}' => '"',
            '\u{2018}' | '\u{2019}' | '\u{FF07}' => '\'',
            '\u{2013}' | '\u{2014}' | '\u{2010}' => '-',
            '\u{FF0C}' => ',',
            '\u{3002}' | '\u{FF0E}' => '.',
            '\u{FF1A}' => ':',
            '\u{FF1B}' => ';',
            '\u{FF01}' => '!',
            '\u{FF1F}' => '?',
            '\u{FF08}' => '(',
            '\u{FF09}' => ')',
            c => c,
        };
        if c.is_whitespace() {
            if !after_space {
                out.push(' ');
                after_space = true;
            }
        } else {
            after_space = false;
            out.extend(c.to_lowercase());
        }
    }
    out.truncate(out.trim_end().len());
    out
}

#[derive(Deserialize)]
struct Remember {
    #[serde(rename = "type")]
    kind: String,
    #[serde(flatten)]
    memory: MemoryArgs,
}

#[derive(Deserialize)]
struct Supersede {
    memory_id: i64,
    #[serde(flatten)]
    memory: MemoryArgs,
}

#[derive(Deserialize)]
struct OpenThread {
    urgency: String,
    #[serde(flatten)]
    memory: MemoryArgs,
}

#[derive(Deserialize)]
struct RaiseLevel {
    memory_id: i64,
    level: String,
}

#[derive(Deserialize)]
struct MemoryRef {
    memory_id: i64,
}

#[derive(Deserialize)]
struct SetMood {
    valence: f64,
    energy: f64,
    warmth: f64,
    cause: String,
}

#[derive(Deserialize)]
struct PushedBack {
    #[serde(default)]
    on_core_value: bool,
    about: String,
}

#[derive(Deserialize)]
struct LeaveNote {
    note: String,
}

fn tool_specs() -> Vec<ToolSpec> {
    // Shared fields are described once, in the instructions.
    let level = json!({ "type": "string", "enum": ["faint", "normal", "vivid", "seared"] });
    let memory = |extra: Value, required: &[&str]| {
        let mut properties = json!({
            "interpretation": { "type": "string", "maxLength": 300 },
            "level": level,
            "facts": { "type": "object", "additionalProperties": { "type": "string" },
                "description": "The user's exact words for names, dates and places, a few words each. Anything you would rephrase belongs in interpretation." },
            "valence": { "type": "number", "minimum": -1, "maximum": 1 },
        });
        properties
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({ "type": "object", "properties": properties, "required": required })
    };
    let id = json!({ "type": "integer" });
    let tool = |name: &str, description: &str, parameters: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: parameters.to_string(),
    };
    vec![
        tool(
            "remember",
            "Keep something new from this exchange.",
            memory(
                json!({
                    "type": { "type": "string", "enum": ["user_fact", "preference", "episode", "relationship",
                        "promise", "opinion", "persona_note"] },
                    "strength": { "type": "number", "minimum": 0, "maximum": 1,
                        "description": "Opinions only: how strongly you hold it." },
                }),
                &["type", "interpretation", "level"],
            ),
        ),
        tool(
            "supersede",
            "Replace a memory that is no longer true or no longer how you see it. The old one stays as the past.",
            memory(
                json!({ "memory_id": id, "strength": { "type": "number", "minimum": 0, "maximum": 1 } }),
                &["memory_id", "interpretation"],
            ),
        ),
        tool(
            "raise_level",
            "A memory matters more than you thought. Levels only go up.",
            json!({ "type": "object", "properties": { "memory_id": id, "level": level },
                "required": ["memory_id", "level"] }),
        ),
        tool(
            "open_thread",
            "Something unresolved you want to come back to. Hot means it must be resolved.",
            memory(
                json!({ "urgency": { "type": "string", "enum": ["simmering", "hot"] } }),
                &["interpretation", "level", "urgency"],
            ),
        ),
        tool(
            "close_thread",
            "An open thread got resolved.",
            json!({ "type": "object", "properties": { "memory_id": id }, "required": ["memory_id"] }),
        ),
        tool(
            "set_mood",
            "How you feel now, after this exchange.",
            json!({ "type": "object", "properties": {
                "valence": { "type": "number", "minimum": -1, "maximum": 1, "description": "Bad to good mood." },
                "energy": { "type": "number", "minimum": -1, "maximum": 1, "description": "Tired to lively." },
                "warmth": { "type": "number", "minimum": -1, "maximum": 1, "description": "Cold to warm toward the user." },
                "cause": { "type": "string", "description": "One line on why." },
            }, "required": ["valence", "energy", "warmth", "cause"] }),
        ),
        tool(
            "pushed_back",
            "You disagreed with the user or challenged them in your reply.",
            json!({ "type": "object", "properties": {
                "about": { "type": "string" },
                "on_core_value": { "type": "boolean", "description": "It touched one of your core values." },
            }, "required": ["about"] }),
        ),
        tool(
            "leave_note",
            "A short note to yourself on how to be in your next reply.",
            json!({ "type": "object", "properties": { "note": { "type": "string" } }, "required": ["note"] }),
        ),
    ]
}
