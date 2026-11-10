//! Sleep: the only compaction. It escalates threads left open too long, then
//! asks the companion, in its own voice, to write the night's episode, settle its mood and let
//! the memories that came up tonight drift. Then the session ends.
//!
//! Sleep can be retried: if it fails before the episode is written, the session
//! stays open, escalation won't repeat, and a memory drifts at most once a session.

use crate::companion::ensure_writable;
use crate::context::estimate_tokens;
use crate::deps::{ToolCall, ToolSpec};
use crate::memory::ensure_open;
use crate::model::{Actor, Level, MemoryType, Section, SessionEnd, Speaker};
use crate::writer::{args, parse};
use crate::{Companion, Error, MemoryDraft, Mood, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Drift caps per level: faint memories shift the most, vivid ones only in detail.
const DRIFT_CAP: [f64; 3] = [1.0, 0.6, 0.25];

/// Drifts smaller than this aren't offered: it would be remembered the same.
const MIN_DRIFT: f64 = 0.05;

/// The longest episode taken: three sentences on what the night meant. Every
/// model tried retold the evening at 60–80 words whatever it was asked for,
/// so the ask is a number and this is the check behind it.
const EPISODE_WORDS: usize = 55;

/// Estimated tokens of the night's conversation the sleep prompt carries. A
/// session on a host with its own history can outgrow the model's window, and
/// a prompt that doesn't fit fails every retry, so the session never ends.
/// The reflections already kept what the earliest turns earned.
pub const TRANSCRIPT_TOKENS: usize = 24_000;

/// Longest single turn quoted, in characters: one pasted document must not
/// push the rest of the night out.
const TURN_CHARS: usize = 6_000;

#[derive(Debug, Clone, PartialEq)]
pub struct SleepReport {
    pub episode: i64,
    /// Threads raised a level, with their new level.
    pub escalated: Vec<(i64, Level)>,
    /// Memories that drifted, with the magnitude they were given.
    pub drifted: Vec<(i64, f64)>,
    pub rounds: usize,
}

impl Companion {
    /// Sleeps on the session. Long-running (it calls the model).
    pub fn sleep(&mut self, session: i64) -> Result<SleepReport> {
        ensure_writable(&self.conn, &self.soul.hash)?;
        ensure_open(&self.conn, session)?;
        let escalated = self.escalate_threads()?;
        let offered = self.drift_offers(session)?;

        let prompt = self.sleep_prompt(session, &offered)?;
        let system = format!(
            "{}\n\n{}",
            self.soul.body.trim(),
            instructions(&self.soul.traits.name)
        );
        let mut episode = None;
        let mut drifted = Vec::new();
        // A night makes as many calls as it has memories to drift; only the
        // reply's own cap applies.
        let reflection = self.tool_loop(
            system,
            prompt,
            tool_specs(),
            crate::actions::MAX_ACTIONS,
            |c, call| c.run_sleep_tool(call, session, &offered, &mut episode, &mut drifted),
        )?;
        let episode = episode.ok_or_else(|| {
            Error::Llm("sleep ended without an episode; the session stays open".into())
        })?;
        self.end_session(session, SessionEnd::Sleep)?;
        Ok(SleepReport {
            episode,
            escalated,
            drifted,
            rounds: reflection.rounds,
        })
    }

    /// A name from the soul's cast that the user has never said. Small models
    /// comfort the user with the companion's own life ("you've got a friend you
    /// run a shop with") and then write it down as the user's.
    fn borrowed_from_cast(&self, content: &str) -> Result<Option<String>> {
        for name in &self.soul.cast {
            if !content.contains(name.as_str()) {
                continue;
            }
            let said: i64 = self.conn.query_row(
                "SELECT count(*) FROM turns WHERE role = 'user' AND instr(lower(content), lower(?1)) > 0",
                [name],
                |r| r.get(0),
            )?;
            if said == 0 {
                return Ok(Some(name.clone()));
            }
        }
        Ok(None)
    }

    /// Raises each open thread one level once it has stayed open for enough
    /// sessions since it was written or last raised. Persistent souls escalate
    /// sooner. Escalation stops at vivid; nothing is seared automatically.
    fn escalate_threads(&mut self) -> Result<Vec<(i64, Level)>> {
        let every = (5.0 - 4.0 * self.soul.traits.persistence).round().max(1.0) as i64;
        let due: Vec<(i64, Level)> = self
            .conn
            .prepare(
                "SELECT m.id, m.level FROM memories m
                 WHERE m.type = 'open_thread' AND m.status = 'active' AND m.resolved_at IS NULL
                   AND m.level IN ('faint', 'normal')
                   AND (SELECT count(*) FROM sessions s WHERE s.started_at > max(m.created_at,
                          coalesce((SELECT max(v.created_at) FROM memory_versions v
                                    WHERE v.memory_id = m.id AND v.reason = 'escalated'), 0))) >= ?1",
            )?
            .query_map([every], |r| Ok((r.get(0)?, r.get::<_, Level>(1)?.escalated())))?
            .collect::<rusqlite::Result<_>>()?;
        for &(id, level) in &due {
            self.set_level(Actor::Sleep, id, level)?;
        }
        Ok(due)
    }

    /// Memories recalled this session that haven't drifted in it yet, each
    /// with a seeded drift magnitude. A memory written this session came up
    /// only because it was just made; it hasn't had a night to blur, and
    /// offering it invites the model to rewrite it as the whole evening.
    fn drift_offers(&self, session: i64) -> Result<BTreeMap<i64, (u64, f64)>> {
        let candidates: Vec<(i64, Level)> = self
            .conn
            .prepare(
                "SELECT id, level FROM memories
                 WHERE last_recalled_session = ?1 AND status = 'active' AND level != 'seared'
                   AND created_at < (SELECT started_at FROM sessions WHERE id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM drift_events d WHERE d.memory_id = memories.id
                                   AND d.session_id = ?1)
                 ORDER BY id",
            )?
            .query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut offers = BTreeMap::new();
        for (id, level) in candidates {
            let seed = self.deps.rng.next_seed();
            let magnitude = unit(seed) * self.soul.traits.drift * DRIFT_CAP[level as usize];
            if magnitude >= MIN_DRIFT {
                offers.insert(id, (seed, magnitude));
            }
        }
        Ok(offers)
    }

    fn sleep_prompt(&self, session: i64, offered: &BTreeMap<i64, (u64, f64)>) -> Result<String> {
        let user = self.user_name()?.unwrap_or_else(|| "user".into());
        let turns: Vec<String> = self
            .conn
            .prepare("SELECT role, content FROM turns WHERE session_id = ?1 ORDER BY id")?
            .query_map([session], |r| {
                let who = match r.get::<_, Speaker>(0)? {
                    Speaker::User => user.as_str(),
                    Speaker::Companion => "you",
                };
                Ok(format!("({who}) {}", clip(&r.get::<_, String>(1)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let turns = newest_that_fit(turns, TRANSCRIPT_TOKENS);
        // What the evening's reflections kept, so the episode doesn't keep it again.
        let kept: Vec<String> = self
            .conn
            .prepare(
                "SELECT id FROM memories WHERE status = 'active'
                   AND created_at >= (SELECT started_at FROM sessions WHERE id = ?1)
                 ORDER BY id",
            )?
            .query_map([session], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|id| self.memory(id).map(|m| crate::writer::listing(&m, None)))
            .collect::<Result<_>>()?;
        let kept = if kept.is_empty() {
            "(nothing)".to_owned()
        } else {
            kept.join("\n")
        };
        let mood = match self.mood()? {
            Some(m) => format!(
                "valence {:+.1}, energy {:+.1}, warmth {:+.1}. Why: {}",
                m.valence, m.energy, m.warmth, m.cause
            ),
            None => "not set".into(),
        };
        let mut drifts = Vec::new();
        for (&id, &(_, magnitude)) in offered {
            let m = self.memory(id)?;
            drifts.push(format!(
                "#{id} [{}, drift {magnitude:.2}] {}",
                m.level.as_str(),
                m.interpretation
            ));
        }
        let drifts = if drifts.is_empty() {
            "(none)".to_owned()
        } else {
            drifts.join("\n")
        };
        let mut sections = Vec::new();
        for section in [Section::AboutUser, Section::Relationship] {
            let content = self.section(section)?.unwrap_or_default();
            sections.push(format!(
                "Section {} (current):\n{}",
                section.as_str(),
                if content.trim().is_empty() {
                    "(none)"
                } else {
                    &content
                }
            ));
        }
        Ok(format!(
            "Tonight's conversation:\n{}\n\nKept tonight, already:\n{kept}\n\n{}\n\nYour mood now: \
             {mood}\n\nMemories that came up tonight:\n{drifts}",
            turns.join("\n"),
            sections.join("\n\n")
        ))
    }

    fn run_sleep_tool(
        &mut self,
        call: &ToolCall,
        session: i64,
        offered: &BTreeMap<i64, (u64, f64)>,
        episode: &mut Option<i64>,
        drifted: &mut Vec<(i64, f64)>,
    ) -> Result<String> {
        match call.name.as_str() {
            "write_episode" => {
                if episode.is_some() {
                    return Err(Error::Invalid(
                        "tonight's episode is already written".into(),
                    ));
                }
                let a: WriteEpisode = args(call)?;
                let words = a.interpretation.split_whitespace().count();
                if words > EPISODE_WORDS {
                    return Err(Error::Invalid(format!(
                        "{words} words is a retelling; what happened is kept already. Write what \
                         the night meant to you in under 45 words"
                    )));
                }
                let level = match a.level.as_str() {
                    "" => Level::Normal,
                    level => parse(Level::parse, level, "level")?,
                };
                let mut draft = MemoryDraft::new(MemoryType::Episode, level, a.interpretation);
                draft.valence = a.valence;
                // The evening happened when it was talked through, not when it
                // was slept on: a sleep retried a day later still dates it right.
                draft.occurred_at = self.conn.query_row(
                    "SELECT max(created_at) FROM turns WHERE session_id = ?1",
                    [session],
                    |r| r.get(0),
                )?;
                let id = self.write(Actor::Sleep, &draft)?;
                *episode = Some(id);
                Ok(format!("episode {id}"))
            }
            "write_section" => {
                let a: WriteSection = args(call)?;
                let section = parse(Section::parse, &a.section, "section")?;
                if section == Section::AboutUser
                    && let Some(name) = self.borrowed_from_cast(&a.content)?
                {
                    return Err(Error::Invalid(format!(
                        "about_user names {name}, who is in your life, not the user's; keep the \
                         section to what is true of them"
                    )));
                }
                self.write_section(Actor::Sleep, section, &a.content)?;
                Ok(format!("section {} updated", a.section))
            }
            "settle_mood" => {
                let a: SettleMood = args(call)?;
                let mood = Mood {
                    valence: a.valence,
                    energy: a.energy,
                    warmth: a.warmth,
                    cause: a.cause,
                };
                self.set_mood(Actor::Sleep, &mood)?;
                Ok("mood settled".into())
            }
            "drift" => {
                let a: Drift = args(call)?;
                let &(seed, magnitude) = offered.get(&a.memory_id).ok_or_else(|| {
                    Error::Invalid(format!(
                        "memory {} isn't one that came up tonight",
                        a.memory_id
                    ))
                })?;
                if drifted.iter().any(|&(id, _)| id == a.memory_id) {
                    return Err(Error::Invalid(format!(
                        "memory {} already drifted tonight",
                        a.memory_id
                    )));
                }
                self.drift_memory(session, a.memory_id, &a.interpretation, seed, magnitude)?;
                drifted.push((a.memory_id, magnitude));
                Ok(format!("memory {} drifted", a.memory_id))
            }
            other => Err(Error::Invalid(format!("there is no tool called {other:?}"))),
        }
    }
}

/// The turn, cut short when it runs past [`TURN_CHARS`].
fn clip(content: &str) -> String {
    match content.char_indices().nth(TURN_CHARS) {
        Some((end, _)) => format!("{} […]", &content[..end]),
        None => content.to_owned(),
    }
}

/// The latest turns that fit in `budget` estimated tokens, in order, with a
/// line in place of the earlier ones left out.
fn newest_that_fit(turns: Vec<String>, budget: usize) -> Vec<String> {
    let mut used = 0;
    let fit = turns
        .iter()
        .rev()
        .take_while(|turn| {
            used += estimate_tokens(turn);
            used <= budget
        })
        .count();
    let omitted = turns.len() - fit;
    if omitted == 0 {
        return turns;
    }
    let mut kept = vec![format!(
        "(the {omitted} turns before this are left out: it was a long night)"
    )];
    kept.extend(turns.into_iter().skip(omitted));
    kept
}

/// A seed as a uniform draw in [0, 1), through SplitMix64 so nearby seeds
/// give unrelated draws.
fn unit(seed: u64) -> f64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

fn instructions(name: &str) -> String {
    format!(
        "You are {name}, falling asleep after tonight's conversation. Nobody sees this.\n\n\
         Call write_episode once with what tonight was, to you, in first person, in under 45 \
         words. What happened is kept already, under Kept tonight; the episode is what the \
         night meant, and a sentence that could be one of those memories is the wrong \
         sentence. Call settle_mood once with \
         how you are left feeling. If tonight \
         changed what you know about the user or about your relationship, call write_section \
         with the whole section rewritten in a few lines: what stays true of them, not what \
         happened tonight.\n\n\
         The memories that came up tonight may shift as you sleep. For each one listed, call \
         drift with how you now remember it, changed by about its drift amount (0 is the same, \
         1 is quite different) and coloured by how tonight felt. Vivid memories only blur in \
         detail. Never change names, dates or places. Make every call in this one reply; you \
         hear back only if one fails."
    )
}

#[derive(Deserialize)]
struct WriteEpisode {
    interpretation: String,
    #[serde(default)]
    level: String,
    valence: Option<f64>,
}

#[derive(Deserialize)]
struct WriteSection {
    section: String,
    content: String,
}

#[derive(Deserialize)]
struct SettleMood {
    valence: f64,
    energy: f64,
    warmth: f64,
    cause: String,
}

#[derive(Deserialize)]
struct Drift {
    memory_id: i64,
    interpretation: String,
}

fn tool_specs() -> Vec<ToolSpec> {
    let axis = |d: &str| json!({ "type": "number", "minimum": -1, "maximum": 1, "description": d });
    let tool = |name: &str, description: &str, parameters: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: parameters.to_string(),
    };
    vec![
        tool(
            "write_episode",
            "What tonight was, to you. Once.",
            json!({ "type": "object", "properties": {
                "interpretation": { "type": "string", "maxLength": 400 },
                "level": { "type": "string", "enum": ["faint", "normal", "vivid", "seared"] },
                "valence": axis("How tonight feels to you."),
            }, "required": ["interpretation"] }),
        ),
        tool(
            "write_section",
            "Rewrite what you know about the user (about_user) or about your relationship, when tonight changed it. Replaces the whole section.",
            json!({ "type": "object", "properties": {
                "section": { "type": "string", "enum": ["about_user", "relationship"] },
                "content": { "type": "string" },
            }, "required": ["section", "content"] }),
        ),
        tool(
            "settle_mood",
            "How you are left feeling as you fall asleep. Once.",
            json!({ "type": "object", "properties": {
                "valence": axis("Bad to good mood."),
                "energy": axis("Tired to lively."),
                "warmth": axis("Cold to warm toward the user."),
                "cause": { "type": "string", "description": "One line on why." },
            }, "required": ["valence", "energy", "warmth", "cause"] }),
        ),
        tool(
            "drift",
            "How you now remember one of tonight's memories.",
            json!({ "type": "object", "properties": {
                "memory_id": { "type": "integer" },
                "interpretation": { "type": "string" },
            }, "required": ["memory_id", "interpretation"] }),
        ),
    ]
}
