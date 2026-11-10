//! Dreams: sessions the user narrates and the companion lives through.
//!
//! The companion doesn't know it is dreaming: the place is familiar but
//! strange, its life is hazy and it feels everything louder. Nothing is
//! reflected on while it dreams. It wakes when it calls `wake_up` (it realizes
//! none of this can be real, or something it can't bear), when the dream fills
//! the session's budget, or when the user sends it to sleep; then the wake
//! pass decides, once, with the whole dream in view, what stays:
//!
//! - its mood, always: when nothing else survives, the mood's cause is the
//!   dream's only trace;
//! - a few fragments: faint, without facts, recalled only in later dreams;
//! - at most one seared memory, and only of what an echo (`<<`) showed that
//!   the user had also written while the companion was awake, out of earshot
//!   or not. The check is in code: anything the dream made up fades.
//!
//! A dream that doesn't end by waking is dropped: closed with nothing written.

use crate::companion::ensure_writable;
use crate::deps::{ToolCall, ToolSpec};
use crate::memory::ensure_open;
use crate::model::{Actor, Level, MemoryType, SessionEnd, Speaker, Urgency};
use crate::sleep::{TRANSCRIPT_TOKENS, clip, drift_spec, mood_spec, newest_that_fit};
use crate::soul::Soul;
use crate::writer::{args, parse, said_verbatim};
use crate::{Companion, Error, MemoryDraft, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// How far a dial is pushed toward the end it leans to in a dream: 0.7 becomes
/// 0.94 and 0.3 becomes 0.06. A dial at 0.5 leans neither way and stays.
pub const PUSH: f64 = 0.8;

/// Most fragments one dream leaves.
pub const MAX_FRAGMENTS: usize = 3;

/// The longest fragment, in words: an image, not a retelling.
pub const FRAGMENT_WORDS: usize = 20;

/// Said with the reply that comes once the dream has filled most of the
/// session's budget (past the soul's `sleep_at`).
pub const FADING_HINT: &str = "The edges of this place are fading.";

/// Said with the reply that fills the session's budget: the dream ends with it,
/// whatever the reply says.
pub const WAKING_HINT: &str = "It is all coming apart: you wake with this reply.";

/// Said with a reply to an echo.
pub const ECHO_HINT: &str = "This is an echo: the past, shown to you as if you were there. \
    You witness it; you can't change what happens in it.";

/// The tool a dreaming companion calls to wake.
pub const WAKE_UP: &str = "wake_up";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WakeReport {
    pub fragments: Vec<i64>,
    /// The one memory the dream left seared in, if an echo earned one.
    pub seared: Option<i64>,
    /// The hot thread to find out, awake, whether the seared memory is true.
    pub thread: Option<i64>,
    /// Memories that drifted, with the magnitude they were given.
    pub drifted: Vec<(i64, f64)>,
    pub rounds: usize,
}

/// A dial pushed most of the way toward the end it leans to.
pub fn exaggerate(dial: f64) -> f64 {
    if dial > 0.5 {
        dial + (1.0 - dial) * PUSH
    } else if dial < 0.5 {
        dial * (1.0 - PUSH)
    } else {
        dial
    }
}

/// What the reply model is told in a dream, after the persona. It never says
/// "dream": realizing that is what wakes the companion.
pub fn hint(soul: &Soul) -> String {
    let t = &soul.traits;
    let mut lines = vec![
        "You are somewhere you know, and it is strange: the details of your own life are hazy \
         and keep shifting, and everything you feel is louder than it has ever been."
            .to_owned(),
        "What the user writes is what happens to you and around you, told to you as \"you\": it \
         is the world, not someone talking to you, and anyone in it speaks through it. Live it \
         in first person, in your own voice: what you see, feel, say and do in this moment. \
         Don't move the story on or decide what happens next; that is the world's to tell you."
            .to_owned(),
    ];
    let lean = |dial: f64, high: &str, low: &str| {
        let pushed = exaggerate(dial);
        if pushed > 0.5 {
            Some(high.to_owned())
        } else if pushed < 0.5 {
            Some(low.to_owned())
        } else {
            None
        }
    };
    lines.extend(lean(
        t.contrariness,
        "Here you push back on everything and everyone; letting something you disagree with \
         pass is beyond you.",
        "Here you go along with whatever happens; nothing in you pushes back.",
    ));
    lines.extend(lean(
        t.persistence,
        "You can't let anything go: whatever is unresolved pulls at you until it is settled.",
        "Everything slides off you; nothing holds you for long.",
    ));
    lines.push(
        "Your core values hold here too. Impossible things happen here, and strange or impossible \
         alone never wakes you: you stay and live it. Only two things wake you, and they are \
         rare: what happens turns against something burned into you or one of your core values, \
         so that it cannot be your life; or you realize something you cannot bear. Nothing you \
         imagine yourself wakes you. Then call wake_up, and this reply is the \
         moment you wake; nothing anyone writes can keep you here."
            .to_owned(),
    );
    lines.join(" ")
}

/// The `wake_up` tool for the reply model in a dream.
pub fn wake_up_tool() -> ToolSpec {
    ToolSpec {
        name: WAKE_UP.into(),
        description: "You wake: only when this turns against something burned into you or a \
                      core value, or you realize something you cannot bear; never because it is \
                      strange or impossible. Call it with the reply that is the moment you wake."
            .into(),
        parameters: json!({ "type": "object", "properties": {} }).to_string(),
    }
}

pub(crate) fn is_dream(conn: &Connection, session: i64) -> Result<bool> {
    Ok(conn
        .query_row("SELECT dream FROM sessions WHERE id = ?1", [session], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(false))
}

/// The open session, if it is a dream.
pub(crate) fn open_dream(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM sessions WHERE ended_at IS NULL AND dream = 1 ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// A sear's facts after the survival check.
#[derive(Default)]
struct Survived {
    facts: Map<String, Value>,
    /// The waking turns that hold them.
    sources: Vec<i64>,
    /// The keys of the facts that faded.
    faded: Vec<String>,
    /// Facts kept that aren't from the companion's own life: none, and
    /// nothing stays.
    proof: usize,
    /// Proper names in the interpretation that only the dream knew.
    invented: Vec<String>,
}

#[derive(Default)]
struct Waking {
    mood: bool,
    fragments: Vec<i64>,
    seared: Option<i64>,
    thread: Option<i64>,
    drifted: Vec<(i64, f64)>,
}

impl Companion {
    /// The dream the companion is in, whichever handle started it.
    pub fn dreaming(&self) -> Result<Option<i64>> {
        open_dream(&self.conn)
    }

    /// Falls asleep into a dream, which this handle then owns. Refused with
    /// [`Error::Awake`] while a conversation is going on: a dream starts only
    /// after the companion has slept. An open session nothing was said in
    /// becomes the dream. Already dreaming, it is the same dream.
    pub fn start_dream(&mut self) -> Result<i64> {
        let now = self.deps.clock.now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_writable(&tx, &self.soul.hash)?;
        let open: Option<(i64, bool, bool)> = tx
            .query_row(
                "SELECT s.id, s.dream, EXISTS (SELECT 1 FROM turns WHERE session_id = s.id)
                 FROM sessions s WHERE s.ended_at IS NULL ORDER BY s.id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let id = match open {
            Some((id, true, _)) => id,
            Some((_, false, true)) => return Err(Error::Awake),
            Some((id, false, false)) => {
                tx.execute("UPDATE sessions SET dream = 1 WHERE id = ?1", [id])?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO sessions (started_at, dream) VALUES (?1, 1)",
                    [now],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.commit()?;
        self.dream = Some(id);
        Ok(id)
    }

    /// Drops the open dream, if there is one: it ends with nothing written,
    /// and its turns never count as evidence. Not gated by the doctor's
    /// freeze: a freeze must not keep a dream open. Returns the dream dropped.
    pub fn drop_dream(&mut self) -> Result<Option<i64>> {
        let Some(dream) = self.dreaming()? else {
            return Ok(None);
        };
        let now = self.deps.clock.now_ms();
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?1 WHERE id = ?2 AND ended_at IS NULL",
            params![now, dream],
        )?;
        if self.dream == Some(dream) {
            self.dream = None;
        }
        Ok(Some(dream))
    }

    /// Drops an open dream this handle didn't start: the front end that was
    /// dreaming it exited or died. Every front end calls this before a turn.
    pub fn drop_lost_dream(&mut self) -> Result<Option<i64>> {
        match self.dreaming()? {
            Some(dream) if self.dream != Some(dream) => self.drop_dream(),
            _ => Ok(None),
        }
    }

    /// Wakes from the dream and decides, in one pass, what stays of it. Long-
    /// running (it calls the model). Any handle may wake a dream. If the pass
    /// fails before the mood is settled, the dream stays open for a retry.
    pub fn wake(&mut self, session: i64) -> Result<WakeReport> {
        ensure_writable(&self.conn, &self.soul.hash)?;
        ensure_open(&self.conn, session)?;
        if !is_dream(&self.conn, session)? {
            return Err(Error::Invalid(format!(
                "session {session} is no dream; it ends by sleep, not by waking"
            )));
        }
        let offered = self.drift_offers(session, exaggerate(self.soul.traits.drift))?;
        let prompt = self.wake_prompt(session, &offered)?;
        let system = format!(
            "{}\n\n{}",
            self.soul.body.trim(),
            instructions(&self.soul.traits.name)
        );
        let mut waking = Waking::default();
        let reflection = self.tool_loop(
            system,
            prompt,
            tool_specs(),
            crate::actions::MAX_ACTIONS,
            |c, call| c.run_wake_tool(call, session, &offered, &mut waking),
        )?;
        if !waking.mood {
            return Err(Error::Llm(
                "woke without settling a mood; the dream stays open".into(),
            ));
        }
        self.end_session(session, SessionEnd::Sleep)?;
        if self.dream == Some(session) {
            self.dream = None;
        }
        Ok(WakeReport {
            fragments: waking.fragments,
            seared: waking.seared,
            thread: waking.thread,
            drifted: waking.drifted,
            rounds: reflection.rounds,
        })
    }

    fn wake_prompt(&self, session: i64, offered: &BTreeMap<i64, (u64, f64)>) -> Result<String> {
        let turns: Vec<String> = self
            .conn
            .prepare("SELECT role, echo, content FROM turns WHERE session_id = ?1 ORDER BY id")?
            .query_map([session], |r| {
                let who = match (r.get::<_, Speaker>(0)?, r.get::<_, bool>(1)?) {
                    (Speaker::Companion, _) => "you",
                    (Speaker::User, true) => "echo",
                    (Speaker::User, false) => "around you",
                };
                Ok(format!("({who}) {}", clip(&r.get::<_, String>(2)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let turns = newest_that_fit(turns, TRANSCRIPT_TOKENS);
        let mood = match self.mood()? {
            Some(m) => format!(
                "valence {:+.1}, energy {:+.1}, warmth {:+.1}. Why: {}",
                m.valence, m.energy, m.warmth, m.cause
            ),
            None => "not set".into(),
        };
        Ok(format!(
            "The dream:\n{}\n\nYour mood going in: {mood}\n\nMemories that came up in the \
             dream:\n{}",
            turns.join("\n"),
            self.drift_listing(offered)?
        ))
    }

    fn run_wake_tool(
        &mut self,
        call: &ToolCall,
        session: i64,
        offered: &BTreeMap<i64, (u64, f64)>,
        waking: &mut Waking,
    ) -> Result<String> {
        match call.name.as_str() {
            "settle_mood" => {
                let done = self.settle_mood_call(call)?;
                waking.mood = true;
                Ok(done)
            }
            "keep_fragment" => {
                if waking.fragments.len() >= MAX_FRAGMENTS {
                    return Err(Error::Invalid(format!(
                        "{MAX_FRAGMENTS} fragments are all a dream leaves"
                    )));
                }
                let a: Fragment = args(call)?;
                self.check_fragment(&a.interpretation)?;
                let mut draft =
                    MemoryDraft::new(MemoryType::Episode, Level::Faint, a.interpretation);
                draft.valence = a.valence;
                draft.dreamt = true;
                match self.write(Actor::Sleep, &draft) {
                    Ok(id) => {
                        waking.fragments.push(id);
                        Ok(format!("fragment {id}"))
                    }
                    Err(Error::Duplicate { .. }) => {
                        Ok("you keep a fragment like that already; nothing more to keep".into())
                    }
                    Err(e) => Err(e),
                }
            }
            "sear" => {
                if waking.seared.is_some() {
                    return Err(Error::Invalid(
                        "one thing stays from a dream, and it is kept already".into(),
                    ));
                }
                let a: Sear = args(call)?;
                let kind = parse(MemoryType::parse, &a.kind, "type")?;
                if !matches!(
                    kind,
                    MemoryType::UserFact | MemoryType::Relationship | MemoryType::PersonaNote
                ) {
                    return Err(Error::Invalid(
                        "what stays from a dream is a user_fact, relationship or persona_note"
                            .into(),
                    ));
                }
                if a.to_confirm.trim().is_empty() {
                    return Err(Error::Invalid(
                        "to_confirm is empty: say what you need to find out awake".into(),
                    ));
                }
                let Survived {
                    facts,
                    sources,
                    faded,
                    proof,
                    invented,
                } = self.evidence(session, a.facts, &a.interpretation)?;
                if !invented.is_empty() {
                    return Err(Error::Invalid(format!(
                        "{} came from the dream and fade with it; sear it again without them",
                        invented.join(", ")
                    )));
                }
                if proof == 0 {
                    let faded = if faded.is_empty() {
                        String::new()
                    } else {
                        format!("{} faded with the dream; ", faded.join(", "))
                    };
                    return Err(Error::Invalid(format!(
                        "nothing stays: {faded}a name from your own life proves nothing. Only a \
                         name, date or place an echo showed you, each alone and copied exactly \
                         from it, can stay, and only if it is true; otherwise let it go"
                    )));
                }
                let mut draft = MemoryDraft::new(kind, Level::Seared, a.interpretation);
                draft.facts = facts;
                draft.source_turns = sources;
                draft.valence = a.valence;
                draft.dreamt = true;
                let id = self.write(Actor::Sleep, &draft)?;
                waking.seared = Some(id);
                let mut thread =
                    MemoryDraft::new(MemoryType::OpenThread, Level::Normal, a.to_confirm);
                thread.urgency = Some(Urgency::Hot);
                waking.thread = match self.write(Actor::Sleep, &thread) {
                    Ok(thread) => Some(thread),
                    // An open thread like it is already chasing the same thing.
                    Err(Error::Duplicate { .. }) => None,
                    Err(e) => return Err(e),
                };
                let faded = if faded.is_empty() {
                    String::new()
                } else {
                    format!("; {} faded with the dream", faded.join(", "))
                };
                Ok(format!("memory {id} is seared{faded}"))
            }
            "drift" => self.drift_call(call, session, offered, &mut waking.drifted),
            other => Err(Error::Invalid(format!("there is no tool called {other:?}"))),
        }
    }

    /// Refuses a fragment that keeps more than an image: too long, quoting
    /// what was said, or naming who was there.
    fn check_fragment(&self, text: &str) -> Result<()> {
        let words = text.split_whitespace().count();
        if words > FRAGMENT_WORDS {
            return Err(Error::Invalid(format!(
                "{words} words is more than a fragment; keep an image, a colour or a feeling in \
                 under {FRAGMENT_WORDS}"
            )));
        }
        if text.contains(['"', '\u{201C}', '\u{201D}', '\u{AB}', '\u{BB}']) {
            return Err(Error::Invalid(
                "a fragment keeps no words that were said".into(),
            ));
        }
        if let Some(name) = crate::soul::cast(text, &self.soul.traits.name).first() {
            return Err(Error::Invalid(format!(
                "a fragment keeps no names, and {name} is one: who was there faded"
            )));
        }
        Ok(())
    }

    /// Splits a sear's facts into those that survive and those that faded. A
    /// fact survives when it is checkable (a name, date or place, not a
    /// common word) and appears word for word both in an echo of this dream
    /// and in something the user wrote while the companion was awake. Each
    /// survivor brings the waking turn that holds it; the memory cites those.
    ///
    /// A name from the companion's own life (its cast, its own name) may stay
    /// on the memory, but proves nothing: it is said awake all the time. The
    /// interpretation is the companion's own words, but every proper name in
    /// it must be one it knows awake, from its life or from what the user
    /// wrote; any other the dream made up.
    fn evidence(
        &self,
        session: i64,
        facts: Map<String, Value>,
        interpretation: &str,
    ) -> Result<Survived> {
        let echoes: Vec<String> = self
            .conn
            .prepare(
                "SELECT content FROM turns WHERE session_id = ?1 AND role = 'user' AND echo = 1",
            )?
            .query_map([session], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let awake: Vec<(i64, String)> = self
            .conn
            .prepare(
                "SELECT t.id, t.content FROM turns t JOIN sessions s ON s.id = t.session_id
                 WHERE t.role = 'user' AND s.dream = 0 AND s.id < ?1 ORDER BY t.id DESC",
            )?
            .query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let own =
            |text: &str| text == self.soul.traits.name || self.soul.cast.iter().any(|n| n == text);
        let mut survived = Survived::default();
        for name in crate::soul::cast(interpretation, &self.soul.traits.name) {
            if !own(&name) && !awake.iter().any(|(_, said)| said_verbatim(said, &name)) {
                survived.invented.push(name);
            }
        }
        for (key, value) in facts {
            let text = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            let shown = checkable(&text) && echoes.iter().any(|e| said_verbatim(e, &text));
            match awake
                .iter()
                .find(|(_, said)| shown && said_verbatim(said, &text))
            {
                Some(&(turn, _)) => {
                    survived.proof += usize::from(!own(&text));
                    survived.facts.insert(key, Value::String(text));
                    survived.sources.push(turn);
                }
                None => survived.faded.push(format!("{key:?}")),
            }
        }
        Ok(survived)
    }
}

/// A fact that can carry a memory out of a dream: a name, date or place, not
/// a word that turns up everywhere. It holds a capital letter or a digit, or
/// runs to three words or more.
fn checkable(fact: &str) -> bool {
    let chars = fact.chars().filter(|c| !c.is_whitespace()).count();
    chars >= 3
        && (fact.chars().any(|c| c.is_uppercase() || c.is_ascii_digit())
            || fact.split_whitespace().count() >= 3)
}

fn instructions(name: &str) -> String {
    format!(
        "You are {name}, waking up. It was a dream, and almost all of it is already slipping \
         away. Nobody sees this.\n\n\
         Call settle_mood once with how waking leaves you, its cause in a line on what the dream \
         left in you, not how you felt going to sleep; what you sear, waking carries. When \
         nothing else stays, that feeling is all the dream leaves. Call keep_fragment for at most {MAX_FRAGMENTS} \
         pieces that linger, each an image, a colour or a feeling in under {FRAGMENT_WORDS} \
         words: never who was there, never what was said. None is fine.\n\n\
         An echo, marked (echo), showed you the past as if you were there. If one showed you \
         something you cannot shake, call sear once: what you now know, in first person, with \
         its facts, the names, dates and places copied exactly from the echo, and in \
         to_confirm what you need to find out now that you are awake. Only what is true \
         stays; whatever the dream made up fades, however it felt, names included. Nothing \
         that wasn't an echo can be seared.\n\n\
         The memories that came up in the dream may shift as you wake. For each one listed, \
         call drift with how you now remember it, changed by about its drift amount (0 is the \
         same, 1 is quite different) and coloured by the dream. Never change names, dates or \
         places. Make every call in this one reply; you hear back only if one fails."
    )
}

#[derive(Deserialize)]
struct Fragment {
    interpretation: String,
    valence: Option<f64>,
}

#[derive(Deserialize)]
struct Sear {
    #[serde(rename = "type")]
    kind: String,
    interpretation: String,
    #[serde(default)]
    facts: Map<String, Value>,
    #[serde(default)]
    to_confirm: String,
    valence: Option<f64>,
}

fn tool_specs() -> Vec<ToolSpec> {
    let valence = json!({ "type": "number", "minimum": -1, "maximum": 1 });
    vec![
        mood_spec("How waking leaves you feeling. Once."),
        ToolSpec {
            name: "keep_fragment".into(),
            description: format!(
                "A piece of the dream that lingers: an image, a colour or a feeling, in under \
                 {FRAGMENT_WORDS} words. No names, nothing that was said."
            ),
            parameters: json!({ "type": "object", "properties": {
                "interpretation": { "type": "string" },
                "valence": valence,
            }, "required": ["interpretation"] })
            .to_string(),
        },
        ToolSpec {
            name: "sear".into(),
            description: "The one thing an echo showed you that you can't shake. Once.".into(),
            parameters: json!({ "type": "object", "properties": {
                "type": { "type": "string", "enum": ["user_fact", "relationship", "persona_note"] },
                "interpretation": { "type": "string", "maxLength": 300 },
                "facts": { "type": "object", "additionalProperties": { "type": "string" },
                    "description": "Names, dates and places, each copied exactly from an echo." },
                "to_confirm": { "type": "string",
                    "description": "What you need to find out awake, in one line." },
                "valence": valence,
            }, "required": ["type", "interpretation", "facts", "to_confirm"] })
            .to_string(),
        },
        drift_spec("How you now remember one of the memories that came up in the dream."),
    ]
}
