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
use crate::{Companion, Error, Memory, MemoryDraft, Mood, Ranking, Result};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Model calls per reflection. A model that keeps calling tools is cut off here.
pub const MAX_ROUNDS: usize = 4;

/// The [`ToolOutcome::name`] recorded for a JSON reply that was refused whole.
pub const INVALID_REPLY: &str = "(invalid reply)";

/// The writer is a careful bookkeeper, not a performer.
const TEMPERATURE: f32 = 0.2;

#[derive(Debug, Clone, PartialEq)]
pub struct Reflection {
    /// Model calls made, at most [`MAX_ROUNDS`].
    pub rounds: usize,
    pub calls: Vec<ToolOutcome>,
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
    pub fn reflect(&mut self, user_turn: i64, companion_turn: i64) -> Result<Reflection> {
        // Fail before paying for a model call.
        ensure_writable(&self.conn, &self.soul.hash)?;
        let said = self.turn_text(user_turn, Speaker::User)?;
        let replied = self.turn_text(companion_turn, Speaker::Companion)?;

        let exchange = [user_turn, companion_turn];
        let prompt = self.reflection_prompt(&exchange, &said, &replied)?;
        let system = reflection_system(&self.soul);
        self.tool_loop(system, prompt, tool_specs(), |c, call| {
            c.run_tool(call, &exchange)
        })
    }

    /// Calls the model with tools for up to [`MAX_ROUNDS`] rounds, running each
    /// call through `run`. One round is the norm: the model is asked for every
    /// call at once and hears back only when one failed, so it can fix it.
    pub(crate) fn tool_loop(
        &mut self,
        system: String,
        prompt: String,
        tools: Vec<ToolSpec>,
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
            for call in &round_calls {
                let outcome = match run(self, call) {
                    Ok(done) => ToolOutcome {
                        name: call.name.clone(),
                        ok: true,
                        result: format!("ok: {done}"),
                    },
                    // A freeze from another process mid-reflection stops it at once.
                    Err(Error::Frozen) => return Err(Error::Frozen),
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
            if !failed {
                break;
            }
            if json {
                messages.push(LlmMessage::user(format!(
                    "Results:\n{}\nReply with actions that fix what failed, or {{\"actions\": []}} \
                     to leave it.",
                    results.join("\n")
                )));
            }
        }
        Ok(Reflection { rounds, calls })
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
    fn reflection_prompt(&self, exchange: &[i64; 2], said: &str, replied: &str) -> Result<String> {
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
        let threads: Vec<String> = self.open_threads()?.iter().map(listing).collect();
        out.push(format!("Open threads:\n{}", or_none(&threads.join("\n"))));
        let recalled = self.recall(&format!("{said}\n{replied}"), &Ranking::default())?;
        let memories: Vec<String> = recalled.iter().map(|r| listing(&r.memory)).collect();
        out.push(format!(
            "Memories this brings up:\n{}",
            or_none(&memories.join("\n"))
        ));
        out.push(format!(
            "The exchange:\nTurn {} (user): {said}\nTurn {} (you): {replied}",
            exchange[0], exchange[1]
        ));
        // Last, where a small model weighs it most: the expected size of the reply.
        out.push("Keep what this earns: one call is usual, none is common, three is a lot.".into());
        Ok(out.join("\n\n"))
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

    /// Runs one tool call and says what happened, for the model.
    fn run_tool(&mut self, call: &ToolCall, exchange: &[i64; 2]) -> Result<String> {
        let w = Actor::Writer;
        match call.name.as_str() {
            "remember" => {
                let a: Remember = args(call)?;
                let kind = parse(MemoryType::parse, &a.kind, "type")?;
                if kind == MemoryType::OpenThread {
                    return Err(Error::Invalid("use open_thread for open threads".into()));
                }
                let level = parse(Level::parse, &a.memory.level, "level")?;
                let draft = a.memory.draft(kind, level, exchange);
                Ok(format!("memory {}", self.write(w, &draft)?))
            }
            "supersede" => {
                let a: Supersede = args(call)?;
                let old = self.memory(a.memory_id)?;
                let level = match a.memory.level.as_str() {
                    "" => old.level,
                    level => parse(Level::parse, level, "level")?,
                };
                let mut draft = a.memory.draft(old.kind, level, exchange);
                draft.urgency = old.urgency;
                Ok(format!(
                    "memory {} replaces {}",
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
                let mut draft = a.memory.draft(MemoryType::OpenThread, level, exchange);
                draft.urgency = Some(parse(Urgency::parse, &a.urgency, "urgency")?);
                Ok(format!("thread {}", self.write(w, &draft)?))
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
                self.record_pushback(w, exchange[1], a.on_core_value, &a.about)?;
                Ok("pushback recorded".into())
            }
            "leave_note" => {
                let a: LeaveNote = args(call)?;
                self.write_turn_note(w, exchange[1], &a.note)?;
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
         unresolved that you would want to ask about next time: open_thread for it, even on a \
         quiet exchange, and close_thread when it resolves. What you know about the user and about \
         your relationship you rewrite when you sleep, with the whole conversation in view, and \
         the evening becomes a memory then too.\n\n\
         Write interpretations in first person, as you felt them, in one or two sentences: what \
         happened and what it meant to you. Don't retell what you replied; the memory keeps the \
         exchange it came from. One memory from an exchange is the norm, a second only for a \
         separate thing worth keeping. A memory's level is how deeply it's burned in: faint, \
         normal, vivid or seared; seared ones never change or fade, so use that rarely. Facts go \
         on user_fact memories only: the names, dates and places that were said, exactly as \
         said; they never change later. Valence is how it feels to you, -1 to 1.\n\n\
         Call set_mood only when this exchange moved your mood, and leave_note only when your \
         next reply needs it, in one line. Make every call you want in this one reply; you hear \
         back only if one fails. Keeping nothing is fine: then call nothing."
    )
}

fn listing(m: &Memory) -> String {
    let facts = if m.facts.is_empty() {
        String::new()
    } else {
        format!(" facts {}", Value::Object(m.facts.clone()))
    };
    format!(
        "#{} [{}, {}] {}{facts}",
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
    /// its facts and re-interpret it from what was really said.
    fn draft(self, kind: MemoryType, level: Level, exchange: &[i64; 2]) -> MemoryDraft {
        let mut draft = MemoryDraft::new(kind, level, self.interpretation);
        draft.source_turns = exchange.to_vec();
        draft.facts = self.facts;
        draft.valence = self.valence;
        draft.strength = self.strength;
        draft
    }
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
                "description": "user_fact only: names, dates and places as said, a few words each." },
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
            "A short note to yourself for your next reply.",
            json!({ "type": "object", "properties": { "note": { "type": "string" } }, "required": ["note"] }),
        ),
    ]
}
