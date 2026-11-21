//! Sleep: the only compaction. It escalates threads left open too long, then
//! asks the companion, in its own voice, to write the night's episode, settle its mood and let
//! the memories that came up tonight drift. Then the session ends.
//!
//! The night ends before sleep runs ([`Companion::close_night`]), so nobody
//! waits on it; sleep closes a night still open itself. It can be retried: if
//! it fails before the episode is written, the night waits for another pass
//! ([`Companion::consolidate`]), escalation won't repeat, and a memory drifts
//! at most once a session.
//!
//! Sleep reads as much of the night as the conversation's allowance
//! ([`Companion::session_tokens`]). When the model call fails before anything
//! ran, it tries once more with half of that: a prompt too large for the model
//! then costs one extra call instead of every retry.

use crate::companion::ensure_writable;
use crate::context::estimate_tokens;
use crate::deps::{ToolCall, ToolSpec};
use crate::diagnostics::Pass;
use crate::memory::{count_failed_pass, heard, mark_consolidated, to_consolidate};
use crate::model::{Actor, Level, MemoryType, Section, Speaker};
use crate::writer::{args, parse};
use crate::{Companion, Error, MemoryDraft, Mood, Reflection, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Instant;

/// Drift caps per level: faint memories shift the most, vivid ones only in detail.
const DRIFT_CAP: [f64; 3] = [1.0, 0.6, 0.25];

/// Drifts smaller than this aren't offered: it would be remembered the same.
const MIN_DRIFT: f64 = 0.05;

/// The longest episode taken: three sentences on the night. Every
/// model tried retold the evening at 60–80 words whatever it was asked for,
/// so the ask is a number and this is the check behind it.
const EPISODE_WORDS: usize = 55;

/// Most memories offered to drift in one night: the ones that would shift
/// most. Each drift is one action, and a long night brings up more memories
/// than one reply can hold alongside the episode, the mood and the sections.
pub const MAX_DRIFTS: usize = 8;

/// Most memories kept during the night that the sleep prompt lists, newest
/// first. A long night keeps hundreds, and the list would crowd out the night.
pub const KEPT_LIMIT: usize = 40;

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
        let started = Instant::now();
        let mut calls = None;
        let report = self.sleep_on(session, &mut calls);
        let error = report.as_ref().err();
        if let Some(e) = error {
            count_failed_pass(&self.conn, session, e);
        }
        self.diagnose_pass(Pass::Sleep, started, false, calls.as_ref(), error);
        report
    }

    /// [`Companion::sleep`]; `calls` is left with the model's calls once it
    /// has made them, whatever comes of the night after.
    fn sleep_on(&mut self, session: i64, calls: &mut Option<Reflection>) -> Result<SleepReport> {
        self.refresh()?;
        ensure_writable(&self.conn, &self.soul.hash)?;
        let open = to_consolidate(&self.conn, session)?;
        if crate::dream::is_dream(&self.conn, session)? {
            return Err(Error::Invalid(format!(
                "session {session} is a dream; it ends by waking, not by sleep"
            )));
        }
        if open {
            self.close_night(session)?;
        }
        let escalated = self.escalate_threads()?;
        let offered = self.drift_offers(session, self.soul.traits.drift)?;

        let system = format!(
            "{}\n\n{}",
            self.soul.body.trim(),
            instructions(&self.soul.traits.name)
        );
        let mut episode = None;
        let mut drifted = Vec::new();
        let mut night = self.session_tokens;
        let reflection = loop {
            let prompt = self.sleep_prompt(session, &offered, night)?;
            let mut ran = false;
            // A section that is refused stays as it was: not worth a round.
            let tried = self.tool_loop(
                Pass::Sleep,
                system.clone(),
                prompt,
                tool_specs(),
                crate::actions::MAX_ACTIONS,
                &["write_section"],
                |c, call| {
                    ran = true;
                    c.run_sleep_tool(call, session, &offered, &mut episode, &mut drifted)
                },
            );
            match tried {
                Err(Error::Llm(_)) if !ran && night == self.session_tokens => night /= 2,
                other => break other?,
            }
        };
        let rounds = reflection.rounds;
        *calls = Some(reflection);
        let episode = episode.ok_or_else(|| {
            Error::Llm("sleep ended without an episode; the night waits for another try".into())
        })?;
        mark_consolidated(&self.conn, session, self.deps.clock.now_ms())?;
        Ok(SleepReport {
            episode,
            escalated,
            drifted,
            rounds,
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
            // A name said only out of earshot was never said to the companion.
            let turns: Vec<String> = self
                .conn
                .prepare(
                    "SELECT content FROM turns WHERE role = 'user' AND instr(lower(content), lower(?1)) > 0",
                )?
                .query_map([name], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let lower = name.to_lowercase();
            if !turns
                .iter()
                .any(|t| heard(t).to_lowercase().contains(&lower))
            {
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
                   AND (SELECT count(*) FROM sessions s WHERE s.dream = 0 AND s.started_at > max(m.created_at,
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
    /// with a seeded drift magnitude under the `drift` dial. A memory written
    /// this session came up only because it was just made; it hasn't had a
    /// night to blur, and offering it invites the model to rewrite it as the
    /// whole evening. What the user told about themselves never blurs: it is
    /// their word, not the companion's memory of it.
    /// At most [`MAX_DRIFTS`], the ones that would shift most.
    pub(crate) fn drift_offers(
        &self,
        session: i64,
        drift: f64,
    ) -> Result<BTreeMap<i64, (u64, f64)>> {
        let candidates: Vec<(i64, Level)> = self
            .conn
            .prepare(
                "SELECT id, level FROM memories
                 WHERE last_recalled_session = ?1 AND status = 'active' AND level != 'seared'
                   AND type NOT IN ('user_fact', 'preference')
                   AND created_at < (SELECT started_at FROM sessions WHERE id = ?1)
                   AND NOT EXISTS (SELECT 1 FROM drift_events d WHERE d.memory_id = memories.id
                                   AND d.session_id = ?1)
                 ORDER BY id",
            )?
            .query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut offers = Vec::new();
        for (id, level) in candidates {
            let seed = self.deps.rng.next_seed();
            let magnitude = unit(seed) * drift * DRIFT_CAP[level as usize];
            if magnitude >= MIN_DRIFT {
                offers.push((id, (seed, magnitude)));
            }
        }
        offers.sort_by(|a, b| b.1.1.total_cmp(&a.1.1).then(a.0.cmp(&b.0)));
        offers.truncate(MAX_DRIFTS);
        Ok(offers.into_iter().collect())
    }

    /// The night's prompt, with the newest `night` estimated tokens of it.
    fn sleep_prompt(
        &self,
        session: i64,
        offered: &BTreeMap<i64, (u64, f64)>,
        night: usize,
    ) -> Result<String> {
        let user = self.user_name()?.unwrap_or_else(|| "user".into());
        let turns: Vec<String> = self
            .conn
            .prepare("SELECT role, content FROM turns WHERE session_id = ?1 ORDER BY id")?
            .query_map([session], |r| {
                let who = match r.get::<_, Speaker>(0)? {
                    Speaker::User => user.as_str(),
                    Speaker::Companion => "you",
                };
                let content: String = r.get(1)?;
                Ok(format!("({who}) {}", clip(&heard(&content))))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let turns = newest_that_fit(turns, night);
        // What the evening's reflections kept, so the episode doesn't keep it
        // again: the newest, as many as the list holds.
        let ids: Vec<i64> = self
            .conn
            .prepare(
                "SELECT id FROM memories WHERE status = 'active'
                   AND created_at >= (SELECT started_at FROM sessions WHERE id = ?1)
                 ORDER BY id",
            )?
            .query_map([session], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<_>>()?;
        let earlier = ids.len().saturating_sub(KEPT_LIMIT);
        let mut kept: Vec<String> = ids[earlier..]
            .iter()
            .map(|&id| self.memory(id).map(|m| crate::writer::listing(&m, None)))
            .collect::<Result<_>>()?;
        if earlier > 0 {
            kept.insert(0, format!("({earlier} kept earlier tonight are left out)"));
        }
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
        let drifts = self.drift_listing(offered)?;
        let told: Vec<String> = self
            .told()?
            .iter()
            .map(|m| crate::writer::listing(m, None))
            .collect();
        let mut sections = vec![format!(
            "What they told you about themselves, kept on its own:\n{}",
            if told.is_empty() {
                "(nothing)".to_owned()
            } else {
                told.join("\n")
            }
        )];
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
                        "{words} words is a retelling; what happened is kept already. Write \
                         tonight in under 45 words: what you talked about and how it left you"
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
                    return Err(Error::Rule(
                        "section_names_cast",
                        format!(
                            "about_user names {name}, who is in your life, not the user's; keep \
                             the section to what is true of them"
                        ),
                    ));
                }
                self.write_section(Actor::Sleep, section, &a.content)?;
                Ok(format!("section {} updated", a.section))
            }
            "supersede" => self.supersede_told(call, session),
            "settle_mood" => self.settle_mood_call(call),
            "drift" => self.drift_call(call, session, offered, drifted),
            other => Err(Error::Invalid(format!("there is no tool called {other:?}"))),
        }
    }

    /// Replaces something the user told about themselves that tonight
    /// changed. Told facts never drift, so a change the reflection missed
    /// would otherwise stay in view on every turn; sleep has the whole night
    /// to catch it. Its facts are checked against tonight's words and those
    /// of the memory it replaces, as a reflection's supersede is.
    fn supersede_told(&mut self, call: &ToolCall, session: i64) -> Result<String> {
        let a: SupersedeTold = args(call)?;
        let old = self.memory(a.memory_id)?;
        if !matches!(old.kind, MemoryType::UserFact | MemoryType::Preference) {
            return Err(Error::Invalid(format!(
                "memory {} is not something they told you about themselves",
                old.id
            )));
        }
        let said: Vec<(i64, String)> = self
            .conn
            .prepare(
                "SELECT id, content FROM turns WHERE session_id = ?1 AND role = 'user' ORDER BY id",
            )?
            .query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let turns: Vec<i64> = said.iter().map(|(id, _)| *id).collect();
        let spoken: Vec<String> = said.iter().map(|(_, text)| heard(text)).collect();
        let earlier = self.source_text(&old, false)?;
        let replaced = Some((old.source_turns.as_slice(), earlier.as_str()));
        let level = match a.memory.level.as_str() {
            "" => old.level,
            level => parse(Level::parse, level, "level")?,
        };
        let (draft, note) =
            a.memory
                .draft(old.kind, level, &turns, &spoken.join("\n"), replaced)?;
        Ok(format!(
            "memory {} replaces {}{note}",
            self.supersede(Actor::Sleep, old.id, &draft)?,
            old.id
        ))
    }

    /// The memories offered to drift, as the model reads them.
    pub(crate) fn drift_listing(&self, offered: &BTreeMap<i64, (u64, f64)>) -> Result<String> {
        let mut drifts = Vec::new();
        for (&id, &(_, magnitude)) in offered {
            let m = self.memory(id)?;
            drifts.push(format!(
                "#{id} [{}, drift {magnitude:.2}] {}",
                m.level.as_str(),
                m.interpretation
            ));
        }
        Ok(if drifts.is_empty() {
            "(none)".to_owned()
        } else {
            drifts.join("\n")
        })
    }

    /// `settle_mood`, falling asleep or waking up.
    pub(crate) fn settle_mood_call(&mut self, call: &ToolCall) -> Result<String> {
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

    /// `drift` on one of the memories `offered`, at most once each.
    pub(crate) fn drift_call(
        &mut self,
        call: &ToolCall,
        session: i64,
        offered: &BTreeMap<i64, (u64, f64)>,
        drifted: &mut Vec<(i64, f64)>,
    ) -> Result<String> {
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
}

/// The turn, cut short when it runs past [`TURN_CHARS`].
pub(crate) fn clip(content: &str) -> String {
    match content.char_indices().nth(TURN_CHARS) {
        Some((end, _)) => format!("{} […]", &content[..end]),
        None => content.to_owned(),
    }
}

/// The latest turns that fit in `budget` estimated tokens, in order, with a
/// line in place of the earlier ones left out.
pub(crate) fn newest_that_fit(turns: Vec<String>, budget: usize) -> Vec<String> {
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
    let limit = crate::state::SECTION_MAX_CHARS;
    format!(
        "You are {name}, falling asleep after tonight's conversation. Nobody sees this.\n\n\
         Call write_episode once with tonight, in first person, in under 45 words: what you \
         talked about, in a few words, and how it left you. The details are kept already, under \
         Kept tonight; don't repeat them, and don't say what the night meant or what it says \
         about them. Call settle_mood once with \
         how you are left feeling. If tonight \
         changed what you know about the user or about your relationship, call write_section \
         with the whole section rewritten in a few lines, under {limit} characters: what stays true of them, as facts and \
         what they have said about themselves, not your reading of them or what happened \
         tonight; what they told you about themselves is kept on its own, so the section \
         doesn't repeat it. If tonight changed one of those things, call supersede on that \
         memory with how it is now, in their words. Unless the user told you their own gender or pronouns, now or \
         before, call them by name or \"they\", never \"he\" or \"she\": the people they talk \
         about don't tell you theirs.\n\n\
         The memories that came up tonight may shift as you sleep. For each one listed, call \
         drift with how you now remember it, changed by about its drift amount (0 is the same, \
         1 is quite different) and coloured by how tonight felt. Drift changes details, emphasis \
         and how sure you are, in plain words like the memory's own; it doesn't add what the \
         memory meant or what it says about them. Vivid memories only blur in \
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
struct SupersedeTold {
    memory_id: i64,
    #[serde(flatten)]
    memory: crate::writer::MemoryArgs,
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

pub(crate) fn tool_specs() -> Vec<ToolSpec> {
    let axis = |d: &str| json!({ "type": "number", "minimum": -1, "maximum": 1, "description": d });
    let tool = |name: &str, description: &str, parameters: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: parameters.to_string(),
    };
    vec![
        tool(
            "write_episode",
            // The limit in words, as the instructions and the check have it: a
            // character limit let a model write 60 words and be refused.
            "Tonight in under 45 words: what you talked about and how it left you. Once.",
            json!({ "type": "object", "properties": {
                "interpretation": { "type": "string" },
                "level": { "type": "string", "enum": ["faint", "normal", "vivid", "seared"] },
                "valence": axis("How tonight feels to you."),
            }, "required": ["interpretation"] }),
        ),
        tool(
            "write_section",
            "Rewrite what you know about the user (about_user) or about your relationship, when tonight changed it. Replaces the whole section. A few lines, under 2000 characters.",
            json!({ "type": "object", "properties": {
                "section": { "type": "string", "enum": ["about_user", "relationship"] },
                "content": { "type": "string", "maxLength": crate::state::SECTION_MAX_CHARS },
            }, "required": ["section", "content"] }),
        ),
        tool(
            "supersede",
            "Something they told you about themselves changed tonight: replace that memory with how it is now.",
            json!({ "type": "object", "properties": {
                "memory_id": { "type": "integer" },
                "interpretation": { "type": "string", "maxLength": 300 },
                "facts": { "type": "object", "additionalProperties": { "type": "string" },
                    "description": "Names, dates and places, each copied exactly from what the user said." },
            }, "required": ["memory_id", "interpretation"] }),
        ),
        mood_spec("How you are left feeling as you fall asleep. Once."),
        drift_spec("How you now remember one of tonight's memories."),
    ]
}

/// `settle_mood`, described for the moment it is called in.
pub(crate) fn mood_spec(description: &str) -> ToolSpec {
    let axis = |d: &str| json!({ "type": "number", "minimum": -1, "maximum": 1, "description": d });
    ToolSpec {
        name: "settle_mood".into(),
        description: description.into(),
        parameters: json!({ "type": "object", "properties": {
            "valence": axis("Bad to good mood."),
            "energy": axis("Tired to lively."),
            "warmth": axis("Cold to warm toward the user."),
            "cause": { "type": "string", "description": "One line on why." },
        }, "required": ["valence", "energy", "warmth", "cause"] })
        .to_string(),
    }
}

/// `drift`, described for the moment it is called in.
pub(crate) fn drift_spec(description: &str) -> ToolSpec {
    ToolSpec {
        name: "drift".into(),
        description: description.into(),
        parameters: json!({ "type": "object", "properties": {
            "memory_id": { "type": "integer" },
            "interpretation": { "type": "string" },
        }, "required": ["memory_id", "interpretation"] })
        .to_string(),
    }
}
