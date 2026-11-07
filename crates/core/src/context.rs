//! Assembling one turn's context, in the design's fixed order and within a
//! token budget:
//!
//! 1. Persona with core values: always included, never truncated.
//! 2. Editable soul sections.
//! 3. Mood.
//! 4. The last turn note.
//! 5. Hot threads.
//! 6. Recalled memories.
//! 7. The current session's turns, newest first, with whatever budget is left.
//!
//! Each part is included only if it fits in what earlier parts left over; a
//! list stops at its first item that doesn't fit, so lower-ranked items go
//! first. Hosts that keep their own history (Hermes) take the parts they need
//! from [`Context`] instead of [`Context::render`].

use crate::companion::meta;
use crate::model::{Level, MemoryType, Section, Speaker, Urgency};
use crate::{Companion, Error, Memory, Mood, Ranking, Recalled, Result};
use rusqlite::OptionalExtension;

/// A deliberately high token estimate (one token per 3 characters), so a
/// budget computed with it doesn't overflow the model's window.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(3)
}

const TIRED_HINT: &str = "You're getting tired. Your replies get shorter.";
const SLEEPY_HINT: &str = "You're very tired. Ask to sleep soon, but don't force it.";
const LET_GO_HINT: &str = "You've pushed back on the user a lot lately. Let the next disagreement go, unless it touches a core value.";

/// Tokens set aside for the hints `render` may add (tiredness, letting go).
pub fn hints_reserve() -> usize {
    estimate_tokens(SLEEPY_HINT).max(estimate_tokens(TIRED_HINT)) + estimate_tokens(LET_GO_HINT)
}

/// Most that time away shifts the companion's warmth, reached after [`TIME_AWAY_FULL_HOURS`].
pub const TIME_AWAY_MAX: f64 = 0.3;
pub const TIME_AWAY_FULL_HOURS: f64 = 72.0;
/// Time away never pushes warmth below this: cooler, never hostile.
pub const TIME_AWAY_FLOOR: f64 = -0.3;
/// Your messages in the new session over which the effect fades out.
pub const TIME_AWAY_FADE_TURNS: f64 = 4.0;

/// How long the user was gone and what it did to the companion's warmth this turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeAway {
    pub hours: f64,
    pub warmth_shift: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub id: i64,
    pub speaker: Speaker,
    pub content: String,
}

/// How tired the companion is, from how much of the session-turn budget the current
/// session already fills. The soul's `tired_at` and `sleep_at` set the steps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fatigue {
    Rested,
    /// Between `tired_at` and `sleep_at`: 0 just past `tired_at`, 1 at `sleep_at`.
    Tired(f64),
    /// Past `sleep_at`: the companion asks to sleep, but never forces it.
    Sleepy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub persona: String,
    pub sections: Vec<(Section, String)>,
    /// The companion's mood, with any time-away shift already applied.
    pub mood: Option<Mood>,
    /// Only for souls with `time_sense`, early in a session after a gap.
    pub time_away: Option<TimeAway>,
    pub turn_note: Option<String>,
    pub hot_threads: Vec<Memory>,
    pub memories: Vec<Recalled>,
    /// Session turns that fit, oldest first. Older ones are dropped first.
    pub turns: Vec<Turn>,
    /// Session turns left out for lack of budget.
    pub omitted_turns: usize,
    pub fatigue: Fatigue,
    /// At the pushback limit: the companion should let the next opinion clash go.
    pub let_go: bool,
    /// Estimated tokens of everything included, the current message and the
    /// [`hints_reserve`].
    pub tokens: usize,
}

impl Companion {
    /// The context for answering `message`, your current message (not yet
    /// recorded), within `budget` estimated tokens. Leave room for the reply
    /// when choosing the budget.
    pub fn assemble(&self, message: &str, budget: usize, ranking: &Ranking) -> Result<Context> {
        if meta(&self.conn, "persona_hash")?.as_deref() != Some(self.soul.hash.as_str()) {
            return Err(Error::PersonaUnresolved);
        }
        let persona = self.soul.body.trim().to_owned();
        let mut left = Budget {
            total: budget,
            used: estimate_tokens(&persona) + estimate_tokens(message),
        };
        let mut fits = |text: &str| left.take(text);

        let mut sections = Vec::new();
        for section in [Section::AboutUser, Section::Relationship] {
            if let Some(content) = self.section(section)?
                && !content.trim().is_empty()
            {
                if !fits(&render_section(section, &content)) {
                    break;
                }
                sections.push((section, content));
            }
        }
        let mut mood = self.mood()?;
        let time_away = match &mut mood {
            Some(m) => self.apply_time_away(m)?,
            None => None,
        };
        let mood = mood.filter(|m| fits(&render_mood(m)));
        let turn_note = self
            .last_turn_note()?
            .map(|(_, note)| note)
            .filter(|n| fits(&render_note(n)));

        let mut hot_threads = Vec::new();
        for memory in self.hot_threads()? {
            if !fits(&render_memory(&memory)) {
                break;
            }
            hot_threads.push(memory);
        }
        let mut memories = Vec::new();
        for recalled in self.recall(message, ranking)? {
            if hot_threads.iter().any(|t| t.id == recalled.memory.id) {
                continue;
            }
            if !fits(&render_memory(&recalled.memory)) {
                break;
            }
            memories.push(recalled);
        }

        // Everything above is fixed for this turn; the session gets the rest.
        let session_budget = left.total.saturating_sub(left.used);
        let session = self.session_turns()?;
        let session_tokens: usize = session.iter().map(|t| estimate_tokens(&t.content)).sum();
        let mut turns = Vec::new();
        for turn in session.iter().rev() {
            if !left.take(&turn.content) {
                break;
            }
            turns.push(turn.clone());
        }
        turns.reverse();

        let traits = &self.soul.traits;
        let fill = if session_budget == 0 {
            f64::INFINITY
        } else {
            session_tokens as f64 / session_budget as f64
        };
        let fatigue = if fill >= traits.sleep_at {
            Fatigue::Sleepy
        } else if fill >= traits.tired_at {
            Fatigue::Tired((fill - traits.tired_at) / (traits.sleep_at - traits.tired_at))
        } else {
            Fatigue::Rested
        };

        let (pushbacks, limit) = self.pushback_pressure()?;
        Ok(Context {
            let_go: pushbacks >= limit,
            time_away,
            persona,
            sections,
            mood,
            turn_note,
            hot_threads,
            omitted_turns: session.len() - turns.len(),
            memories,
            turns,
            fatigue,
            tokens: left.used,
        })
    }

    /// For a host that keeps its own history and replays every message, so
    /// whatever one turn injects is paid for again on every later turn: drops
    /// from `ctx` what the model already saw earlier in the open session. The
    /// first turn gets everything; later ones only what changed since the last
    /// time you spoke, plus memories not yet recalled this session. Hot threads
    /// and memories count as seen once [`Companion::mark_recalled`] records them.
    pub fn keep_new(&self, ctx: &mut Context) -> Result<()> {
        let last: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT session_id, max(created_at) FROM turns WHERE role = 'user'
                 AND session_id = (SELECT id FROM sessions WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1)",
                [],
                |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
            )
            .map(|(s, t)| s.zip(t))?;
        let Some((session, since)) = last else {
            return Ok(());
        };
        let changed = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Result<bool> {
            let newest: Option<i64> = self.conn.query_row(sql, params, |r| r.get(0))?;
            Ok(newest.is_some_and(|t| t >= since))
        };
        let mut sections = Vec::new();
        for (section, content) in std::mem::take(&mut ctx.sections) {
            if changed(
                "SELECT max(created_at) FROM soul_section_versions WHERE name = ?1",
                &[&section],
            )? {
                sections.push((section, content));
            }
        }
        ctx.sections = sections;
        // A time-away shift is this turn's own; otherwise the mood is as last shown.
        if ctx.time_away.is_none() && !changed("SELECT max(created_at) FROM mood_states", &[])? {
            ctx.mood = None;
        }
        if !changed("SELECT max(created_at) FROM turn_notes", &[])? {
            ctx.turn_note = None;
        }
        ctx.hot_threads
            .retain(|m| m.last_recalled_session != Some(session));
        ctx.memories
            .retain(|r| r.memory.last_recalled_session != Some(session));
        Ok(())
    }

    /// Shifts warmth by how long the user was gone: warmer if the last session
    /// ended on good terms, cooler if not. Fades over your first messages back.
    fn apply_time_away(&self, mood: &mut Mood) -> Result<Option<TimeAway>> {
        if !self.soul.traits.time_sense {
            return Ok(None);
        }
        let ended: Option<i64> = self.conn.query_row(
            "SELECT max(ended_at) FROM sessions WHERE ended_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        let Some(ended) = ended else { return Ok(None) };
        let hours = (self.deps.clock.now_ms() - ended) as f64 / 3_600_000.0;
        let said: i64 = self.conn.query_row(
            "SELECT count(*) FROM turns WHERE role = 'user' AND session_id =
               (SELECT id FROM sessions WHERE ended_at IS NULL AND started_at > ?1 ORDER BY id DESC LIMIT 1)",
            [ended],
            |r| r.get(0),
        )?;
        let fade = 1.0 - said as f64 / TIME_AWAY_FADE_TURNS;
        let parted: Option<f64> = self
            .conn
            .query_row(
                "SELECT valence FROM mood_states WHERE created_at <= ?1 ORDER BY id DESC LIMIT 1",
                [ended],
                |r| r.get(0),
            )
            .optional()?;
        let Some(parted) = parted else {
            return Ok(None);
        };
        if hours < 1.0 || fade <= 0.0 {
            return Ok(None);
        }
        let strength = TIME_AWAY_MAX * (hours / TIME_AWAY_FULL_HOURS).min(1.0) * fade;
        let before = mood.warmth;
        mood.warmth = if parted >= 0.0 {
            (before + strength).min(1.0)
        } else {
            (before - strength).max(before.min(TIME_AWAY_FLOOR))
        };
        mood.cause = format!(
            "{} It's been about {hours:.0} hours since you last talked.",
            mood.cause
        );
        Ok(Some(TimeAway {
            hours,
            warmth_shift: mood.warmth - before,
        }))
    }

    /// Unresolved hot threads, oldest first: they must come up until closed.
    fn hot_threads(&self) -> Result<Vec<Memory>> {
        let ids: Vec<i64> = self
            .conn
            .prepare(
                "SELECT id FROM memories
                 WHERE type = ?1 AND urgency = ?2 AND status = 'active' AND resolved_at IS NULL
                 ORDER BY id",
            )?
            .query_map(
                rusqlite::params![MemoryType::OpenThread, Urgency::Hot],
                |r| r.get(0),
            )?
            .collect::<rusqlite::Result<_>>()?;
        ids.into_iter().map(|id| self.memory(id)).collect()
    }

    fn session_turns(&self) -> Result<Vec<Turn>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, content FROM turns
             WHERE session_id = (SELECT id FROM sessions WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1)
             ORDER BY id",
        )?;
        let turns = stmt.query_map([], |r| {
            Ok(Turn {
                id: r.get(0)?,
                speaker: r.get(1)?,
                content: r.get(2)?,
            })
        })?;
        Ok(turns.collect::<rusqlite::Result<_>>()?)
    }
}

struct Budget {
    total: usize,
    used: usize,
}

impl Budget {
    /// Spends the text's tokens if they fit.
    fn take(&mut self, text: &str) -> bool {
        let cost = estimate_tokens(text);
        let fits = self.used + cost <= self.total;
        if fits {
            self.used += cost;
        }
        fits
    }
}

impl Context {
    /// Parts 1–6 as one text, for the system prompt of a host that sends
    /// [`Context::turns`] as chat messages itself.
    pub fn render(&self) -> String {
        let mut out = vec![self.persona.clone()];
        out.extend(self.sections.iter().map(|(s, c)| render_section(*s, c)));
        out.extend(self.mood.iter().map(render_mood));
        out.extend(self.turn_note.iter().map(|n| render_note(n)));
        if !self.hot_threads.is_empty() {
            let threads: Vec<String> = self.hot_threads.iter().map(render_memory).collect();
            out.push(format!(
                "Unresolved, and it matters to you:\n{}",
                threads.join("\n")
            ));
        }
        if !self.memories.is_empty() {
            let memories: Vec<String> = self
                .memories
                .iter()
                .map(|r| render_memory(&r.memory))
                .collect();
            out.push(format!("What you remember:\n{}", memories.join("\n")));
        }
        match self.fatigue {
            Fatigue::Rested => {}
            Fatigue::Tired(_) => out.push(TIRED_HINT.into()),
            Fatigue::Sleepy => out.push(SLEEPY_HINT.into()),
        }
        if self.let_go {
            out.push(LET_GO_HINT.into());
        }
        out.join("\n\n")
    }
}

fn render_section(section: Section, content: &str) -> String {
    let title = match section {
        Section::AboutUser => "About the user",
        Section::Relationship => "Your relationship",
    };
    format!("## {title}\n{}", content.trim())
}

fn render_mood(m: &Mood) -> String {
    format!(
        "Your mood: valence {:+.1}, energy {:+.1}, warmth toward the user {:+.1}. Why: {}",
        m.valence, m.energy, m.warmth, m.cause
    )
}

fn render_note(note: &str) -> String {
    format!("Your note from last turn: {note}")
}

fn render_memory(m: &Memory) -> String {
    let mut line = format!("- {}", m.interpretation);
    if m.level == Level::Seared {
        line.push_str(" (this one is burned into you)");
    }
    if !m.facts.is_empty() {
        let facts: Vec<String> = m
            .facts
            .iter()
            .map(|(k, v)| format!("{k}: {}", v.as_str().map_or(v.to_string(), str::to_owned)))
            .collect();
        line.push_str(&format!(" [{}]", facts.join(", ")));
    }
    line
}
