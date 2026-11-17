//! What clanka did, in numbers and fixed words: enough to find a bug in real
//! use without reading anyone's conversation.
//!
//! An [`Event`] cannot hold what was said, remembered or written by a model:
//! it has no owned string, only numbers, yes/no and `&'static str` words this
//! code spells out. Add a field of any other kind and that stops being true.
//!
//! The core says what happened; where events go is the front end's choice
//! (the CLI appends them to a local file, and nothing leaves the machine).

use crate::Error;
use serde::Serialize;
use std::time::Instant;
use uuid::Uuid;

/// Where a handle's events go.
pub trait Recorder: Send + Sync {
    /// `companion` is whose memory the event is about, when it is about one.
    /// Never fails: a log that can't be written must not cost a turn.
    fn record(&self, companion: Option<Uuid>, event: &Event);
}

/// Records nothing.
pub struct Silent;

impl Recorder for Silent {
    fn record(&self, _: Option<Uuid>, _: &Event) {}
}

/// A pass the companion's model makes over memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pass {
    /// On one exchange, after the reply.
    Reflect,
    /// On a whole conversation, at its end.
    Sleep,
    /// On a dream, when the companion wakes.
    Wake,
    /// The doctor's, on memories it names.
    Reinterpret,
}

/// Prose tics in one reply, by kind (see [`crate::tics`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Tics {
    pub negation: usize,
    pub label: usize,
    pub fragments: usize,
}

impl Tics {
    pub fn of(reply: &str) -> Self {
        let mut tics = Self::default();
        for tic in crate::tics::in_prose(reply) {
            match tic.kind {
                crate::tics::Kind::Negation => tics.negation += 1,
                crate::tics::Kind::Label => tics.label += 1,
                crate::tics::Kind::Fragments => tics.fragments += 1,
                // Looked for in personas only.
                crate::tics::Kind::Maxim => {}
            }
        }
        tics
    }
}

/// A call the model made in a pass that was refused. Written as `tool/code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    /// Which tool, or `unknown` for one that isn't offered.
    pub tool: &'static str,
    /// Why: an [`Error::code`], [`INVALID_REPLY`] or [`TOO_MANY_CALLS`].
    pub code: &'static str,
}

impl Serialize for Refused {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{}/{}", self.tool, self.code))
    }
}

/// The reply wasn't the JSON actions a text-only model is asked for.
pub const INVALID_REPLY: &str = "invalid_reply";
/// The call came after the most one pass may make, and never ran.
pub const TOO_MANY_CALLS: &str = "too_many_calls";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A `clanka` command ran to its end.
    Command {
        name: &'static str,
        ms: u64,
        /// `ok`, `found` (it worked and found something wrong) or `error`.
        outcome: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'static str>,
    },
    /// A front end opened a handle to a companion.
    Open {
        ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'static str>,
    },
    /// A turn began: the user's message was checked, recalled on and recorded.
    TurnBegin {
        ms: u64,
        /// `open`, `doctor`, `frozen` or `awake`; the numbers below are of an
        /// `open` turn and 0 otherwise.
        outcome: &'static str,
        /// The host conversation's first turn, which gets the whole context.
        first: bool,
        dream: bool,
        /// The message said goodnight with `zz`.
        goodnight: bool,
        echo: bool,
        /// Something in it was said out of earshot.
        unheard: bool,
        /// Paragraphs a `>>N` asked for.
        #[serde(skip_serializing_if = "Option::is_none")]
        stretch: Option<u32>,
        message_chars: usize,
        /// Estimated tokens of the whole context, against the turn's budget.
        context_tokens: usize,
        /// Memories recall brought up, before a replaying host's are left out.
        memories: usize,
        hot_threads: usize,
        /// Session turns that no longer fit.
        omitted_turns: usize,
        /// The session fills its budget.
        spent: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'static str>,
    },
    /// A turn ended: the reply was recorded.
    TurnEnd {
        ms: u64,
        reply_chars: usize,
        /// The reply had em dashes to take out.
        dashes: bool,
        tics: Tics,
        /// Memories marked as recalled this turn.
        recalled: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'static str>,
    },
    /// The reply model searched memory on purpose.
    Recall {
        ms: u64,
        found: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'static str>,
    },
    /// The model made a pass over memory.
    MemoryPass {
        pass: Pass,
        ms: u64,
        /// A reflection made up for late, after its first try failed.
        late: bool,
        /// Model calls made.
        rounds: usize,
        /// Calls that ran.
        ok: usize,
        refused: Vec<Refused>,
        /// Writes that dropped facts for not being the user's words.
        left_out: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'static str>,
    },
    /// One model call of a pass.
    ModelCall {
        pass: Pass,
        ms: u64,
        prompt_chars: usize,
        reply_chars: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'static str>,
    },
    /// Something a host plugin saw that the core can't: see [`PLUGIN_EVENTS`].
    Plugin { what: &'static str },
}

/// What a host plugin may report, since it can only pass a string: memory did
/// not open in time and the turn was blocked, memory could not take the turn
/// and it was blocked, and a goodnight could not be sent for the user.
pub const PLUGIN_EVENTS: [&str; 3] = ["open_timeout", "turn_blocked", "goodnight_not_sent"];

/// Every tool a pass offers the model. A model can call anything, so a call's
/// name goes into an event only as one of these.
const TOOLS: [&str; 15] = [
    "remember",
    "supersede",
    "raise_level",
    "open_thread",
    "close_thread",
    "set_mood",
    "pushed_back",
    "leave_note",
    "write_episode",
    "write_section",
    "settle_mood",
    "drift",
    "keep_fragment",
    "sear",
    "reinterpret",
];

/// The tool a model called, as the word an event may hold.
pub fn tool(name: &str) -> &'static str {
    TOOLS
        .iter()
        .find(|known| **known == name)
        .copied()
        .unwrap_or("unknown")
}

/// Whole milliseconds since `started`.
pub fn ms_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl Error {
    /// Which kind of error this is, as a word an [`Event`] may hold. The
    /// message stays out: a provider's error can quote the prompt, and
    /// SQLite's can quote a query.
    pub fn code(&self) -> &'static str {
        use rusqlite::ErrorCode;
        match self {
            Error::WrongKey(_) => "wrong_key",
            Error::Sqlite(e) => match e.sqlite_error_code() {
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => "sqlite_busy",
                Some(ErrorCode::ConstraintViolation) => "sqlite_constraint",
                Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => "sqlite_corrupt",
                Some(ErrorCode::DiskFull) => "sqlite_full",
                Some(ErrorCode::ReadOnly | ErrorCode::CannotOpen | ErrorCode::SystemIoFailure) => {
                    "sqlite_io"
                }
                _ => "sqlite",
            },
            Error::SchemaTooNew { .. } => "schema_too_new",
            Error::NoWal { .. } => "no_wal",
            Error::Io { .. } => "io",
            Error::Soul(_) => "soul",
            Error::CompanionMismatch { .. } => "companion_mismatch",
            Error::EmbedderChanged { .. } => "embedder_changed",
            Error::Frozen => "frozen",
            Error::NotFrozen => "not_frozen",
            Error::Archived => "archived",
            Error::PersonaUnresolved => "persona_unresolved",
            Error::SessionClosed(_) => "session_closed",
            Error::Awake => "awake",
            Error::NotFound(_) => "not_found",
            Error::Duplicate { .. } => "duplicate",
            Error::LevelDown { .. } => "level_down",
            Error::Seared(_) => "seared",
            Error::Invalid(_) => "invalid",
            Error::Key(_) => "key",
            Error::Embedder(_) => "embedder",
            Error::Llm(_) => "llm",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool missing from the table would be logged as `unknown`.
    #[test]
    fn every_offered_tool_has_its_word() {
        let offered = [
            crate::writer::tool_specs(),
            crate::sleep::tool_specs(),
            crate::dream::tool_specs(),
            crate::doctor::tool_specs(),
        ];
        for spec in offered.iter().flatten() {
            assert_eq!(tool(&spec.name), spec.name);
        }
        assert_eq!(tool("my mother's name is Ana"), "unknown");
    }
}
