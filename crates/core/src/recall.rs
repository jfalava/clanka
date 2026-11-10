//! Retrieval on the hot path: hybrid full-text and vector search, ranked by
//! relevance, level, burial, reinforcement and mood.
//!
//! Recall is read-only, so it never waits on another process's write lock.
//! Reinforcement is recorded separately with [`Companion::mark_recalled`], off
//! the hot path, after the reply.

use crate::companion::{ensure_writable, log};
use crate::model::{Actor, Level, MemoryType};
use crate::{Companion, Error, Memory, Result};
use rusqlite::{TransactionBehavior, params};
use serde_json::json;

/// The tuning surface for recall. The defaults are first guesses; tune them
/// against real conversations.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ranking {
    /// Most memories returned per turn.
    pub limit: usize,
    /// Most seared memories per turn, so the same moments don't come up forever.
    pub seared_limit: usize,
    /// Below this relevance a memory is never recalled, seared or not.
    pub min_relevance: f64,
    /// Below this final score a memory has faded out of reach.
    pub min_score: f64,
    /// Added to the cosine relevance for the best full-text match, scaled
    /// down for weaker matches. Lets names and places in `facts` count.
    pub fts_weight: f64,
    /// How strongly the companion's mood pulls same-feeling memories up and others down.
    pub mood_weight: f64,
    /// Multiplier for open threads that haven't been resolved.
    pub thread_boost: f64,
    /// Newer memories after which a faint memory's rank halves.
    pub faint_half_life: f64,
    /// Same for normal and vivid memories.
    pub normal_half_life: f64,
    /// Vivid memories never sink below this burial factor.
    pub vivid_floor: f64,
    /// Weight per level: faint, normal, vivid, seared.
    pub level_weights: [f64; 4],
}

impl Default for Ranking {
    fn default() -> Self {
        Self {
            limit: 8,
            seared_limit: 2,
            min_relevance: 0.25,
            min_score: 0.05,
            fts_weight: 0.15,
            mood_weight: 0.15,
            thread_boost: 1.2,
            faint_half_life: 40.0,
            normal_half_life: 400.0,
            vivid_floor: 0.35,
            level_weights: [0.8, 1.0, 1.15, 1.3],
        }
    }
}

impl Ranking {
    /// Refuses values that would make recall fail or rank nonsense: a number
    /// that isn't one, a negative weight, a half-life that isn't above zero.
    pub fn check(&self) -> Result<()> {
        for (name, value) in [
            ("min_relevance", self.min_relevance),
            ("min_score", self.min_score),
        ] {
            if !value.is_finite() {
                return Err(Error::Invalid(format!("recall {name} is {value}")));
            }
        }
        let at_least_zero = [
            ("fts_weight", self.fts_weight),
            ("mood_weight", self.mood_weight),
            ("thread_boost", self.thread_boost),
            ("vivid_floor", self.vivid_floor),
            ("level_weights[0]", self.level_weights[0]),
            ("level_weights[1]", self.level_weights[1]),
            ("level_weights[2]", self.level_weights[2]),
            ("level_weights[3]", self.level_weights[3]),
        ];
        let above_zero = [
            ("faint_half_life", self.faint_half_life),
            ("normal_half_life", self.normal_half_life),
        ];
        for (name, value) in at_least_zero {
            if !(value.is_finite() && value >= 0.0) {
                return Err(Error::Invalid(format!(
                    "recall {name} is {value}; it must be 0 or more"
                )));
            }
        }
        for (name, value) in above_zero {
            if !(value.is_finite() && value > 0.0) {
                return Err(Error::Invalid(format!(
                    "recall {name} is {value}; it must be above 0"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recalled {
    pub memory: Memory,
    /// Cosine similarity plus the full-text bonus.
    pub relevance: f64,
    pub score: f64,
}

struct Candidate {
    id: i64,
    kind: MemoryType,
    level: Level,
    valence: Option<f64>,
    recall_count: i64,
    resolved: bool,
    cosine: f64,
    bm25: Option<f64>,
}

impl Companion {
    /// Memories for the turn about to happen. `message` is your current message,
    /// not yet recorded; the query adds your previous user turns from the open
    /// session, up to the soul's `recall_window` messages in all.
    pub fn recall(&self, message: &str, ranking: &Ranking) -> Result<Vec<Recalled>> {
        self.search(&self.recall_query(message)?, ranking)
    }

    /// What `query` alone brings up, whatever was said in the open session:
    /// [`Companion::recall`] without the earlier messages. For looking into
    /// recall and measuring a [`Ranking`]; a turn uses `recall`.
    pub fn search(&self, query: &str, ranking: &Ranking) -> Result<Vec<Recalled>> {
        ranking.check()?;
        let embedding = self.embed(query)?;
        let mood = self.mood()?.map(|m| m.valence);
        let newest: i64 =
            self.conn
                .query_row("SELECT coalesce(max(id), 0) FROM memories", [], |r| {
                    r.get(0)
                })?;

        let fts = fts_query(query);
        let fts_cte = if fts.is_some() {
            "SELECT rowid AS id, bm25(memories_fts) AS rank FROM memories_fts
             WHERE memories_fts MATCH ?2 ORDER BY rank LIMIT 64"
        } else {
            "SELECT NULL AS id, NULL AS rank WHERE 0 AND ?2 IS NULL"
        };
        // Every active memory is a candidate, each scored against the query:
        // nearest-neighbour search runs before any filter, so superseded
        // memories took its slots and a full-text match outside them was lost.
        // vec0 has no index, so both read every vector; scoring them all here
        // is slower (about 26 ms against 3 ms at 3,000 memories) and still
        // far below what a turn's model call takes.
        let sql = format!(
            "WITH fts AS MATERIALIZED ({fts_cte})
             SELECT m.id, m.type, m.level, m.valence, m.recall_count, m.resolved_at IS NOT NULL,
                    1 - vec_distance_cosine(v.embedding, ?1), fts.rank
             FROM memories m
             JOIN memory_vectors v ON v.memory_id = m.id
             LEFT JOIN fts ON fts.id = m.id
             WHERE m.status = 'active'"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let candidates = stmt
            .query_map(params![embedding, fts], |r| {
                Ok(Candidate {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    level: r.get(2)?,
                    valence: r.get(3)?,
                    recall_count: r.get(4)?,
                    resolved: r.get(5)?,
                    cosine: r.get(6)?,
                    bm25: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // bm25 is negative, lower is better; scale each match against the best one.
        let best = candidates.iter().filter_map(|c| c.bm25).fold(0.0, f64::min);
        let mut scored: Vec<(f64, f64, &Candidate)> = candidates
            .iter()
            .filter_map(|c| {
                let fts = c
                    .bm25
                    .map_or(0.0, |rank| if best < 0.0 { rank / best } else { 1.0 });
                let relevance = c.cosine.max(0.0) + ranking.fts_weight * fts;
                if relevance < ranking.min_relevance {
                    return None;
                }
                let score = relevance * weigh(c, newest, mood, ranking);
                (score >= ranking.min_score).then_some((score, relevance, c))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));

        let mut seared = 0;
        let mut recalled = Vec::new();
        for (score, relevance, c) in scored {
            if recalled.len() == ranking.limit {
                break;
            }
            if c.level == Level::Seared {
                if seared == ranking.seared_limit {
                    continue;
                }
                seared += 1;
            }
            recalled.push(Recalled {
                memory: self.memory(c.id)?,
                relevance,
                score,
            });
        }
        Ok(recalled)
    }

    /// Records that these memories came up in a session: each session a memory
    /// comes up in slows its burial, however many turns of it, and sleep drifts
    /// only memories recalled that session.
    pub fn mark_recalled(&mut self, session: i64, ids: &[i64]) -> Result<()> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        for id in ids {
            tx.execute(
                "UPDATE memories SET recall_count = recall_count + (last_recalled_session IS NOT ?1),
                   last_recalled_session = ?1
                 WHERE id = ?2",
                [session, *id],
            )?;
        }
        log(
            &tx,
            Actor::Writer.as_str(),
            "recalled",
            "memory",
            None,
            &json!({ "ids": ids }),
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    fn recall_query(&self, message: &str) -> Result<String> {
        let previous = i64::from(self.soul.traits.recall_window) - 1;
        let mut stmt = self.conn.prepare(
            "SELECT content FROM turns
             WHERE role = 'user'
               AND session_id = (SELECT id FROM sessions WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1)
             ORDER BY id DESC LIMIT ?1",
        )?;
        let mut parts = stmt
            .query_map([previous], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        parts.reverse();
        parts.push(message.to_owned());
        Ok(parts.join("\n"))
    }
}

/// Everything but relevance: level, burial by volume, reinforcement, mood and
/// open threads.
fn weigh(c: &Candidate, newest: i64, mood: Option<f64>, r: &Ranking) -> f64 {
    let age = (newest - c.id) as f64 / (1.0 + c.recall_count as f64);
    let half = |life: f64| 0.5_f64.powf(age / life);
    let (weight, burial) = match c.level {
        Level::Faint => (r.level_weights[0], half(r.faint_half_life)),
        Level::Normal => (r.level_weights[1], half(r.normal_half_life)),
        Level::Vivid => (
            r.level_weights[2],
            half(r.normal_half_life).max(r.vivid_floor),
        ),
        Level::Seared => (r.level_weights[3], 1.0),
    };
    let mood = match (mood, c.valence) {
        (Some(mood), Some(memory)) => 1.0 + r.mood_weight * mood * memory,
        _ => 1.0,
    };
    let thread = if c.kind == MemoryType::OpenThread && !c.resolved {
        r.thread_boost
    } else {
        1.0
    };
    weight * burial * mood * thread
}

/// An FTS5 OR-query of the message's words, each quoted so punctuation and
/// operators in what you type can't break the query. Words under 3 characters
/// are dropped.
fn fts_query(text: &str) -> Option<String> {
    let mut words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    words.sort();
    words.dedup();
    words.truncate(32);
    (!words.is_empty()).then(|| {
        words
            .iter()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(" OR ")
    })
}
