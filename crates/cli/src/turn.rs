//! One exchange as a host sees it: `begin` before the model replies, `end`
//! after. Shared by every front end (MCP, the Hermes extension, and `clanka
//! chat` through `open`), so the doctor token and freeze behave the same in all.
//!
//! Such a host replays everything it was given on every later turn, so a turn
//! carries only what the model hasn't seen this session: the first turn gets
//! the whole context, later ones only what changed and memories new tonight.
//!
//! Markers at the start of a message, parsed here so every front end reads
//! them the same, in this order: `zz` says goodnight (the companion sleeps
//! after this reply, or wakes from a dream), `~~` falls asleep into a dream
//! (only as the first message after sleep), `<<` shows the dreaming companion
//! an echo of the past, `>>N` asks for a reply of about N paragraphs.
//! Anywhere in a waking message, text between a pair of `||` is said out of
//! earshot.

use clanka_core::diagnostics::{Event, Tics, ms_since};
use clanka_core::dream::{ECHO_HINT, WAKE_UP};
use clanka_core::memory::has_unheard;
use clanka_core::model::Speaker;
use clanka_core::{Companion, Context, Error, Recalled, Result};
use std::time::Instant;

/// The turn `begin` opened and `end` closes.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub session: i64,
    pub user_turn: i64,
    /// Memories and hot threads shown this turn.
    pub recalled: Vec<i64>,
    /// The turn is part of a dream: nothing is reflected on, and the reply
    /// may wake the companion (the `wake_up` tool).
    pub dream: bool,
    /// The dream ends with this reply: it fills the session's budget, or the
    /// companion's `wake_up` woke it. The front end wakes the companion once
    /// the reply is recorded.
    pub wake: bool,
    /// The user said goodnight with `zz`: the front end puts the companion to
    /// sleep once the reply is recorded, with no reflection on it first.
    pub sleep: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Begin {
    /// The message was the doctor token: memory is now frozen and nothing was
    /// recorded. The host should not pass the message to the model.
    Doctor,
    /// Memory is frozen by the doctor: nothing is recorded or recalled.
    Frozen,
    /// The message asked for a dream (`~~`) while the companion is awake in a
    /// conversation. Nothing was recorded; the host should not pass the
    /// message to the model, and can show [`AWAKE`].
    Awake,
    /// The turn is recorded; `context` is the memory to give the model, empty
    /// when nothing is new since the last turn.
    Open { context: String, pending: Pending },
}

/// Why a `~~` was refused.
pub const AWAKE: &str = "Still awake in this conversation: a dream starts with ~~ only as the \
    first message after sleep.";

/// What a bare `~~` puts in the dream: somewhere to be.
pub const DREAM_OPENING: &str = "You are somewhere you almost know.";

/// Told to the reply model when the user wrote something out of the
/// companion's earshot. The model knows it, so the story holds together; the
/// companion never acts as if it heard.
pub const UNHEARD_HINT: &str = "Words between || were said out of your earshot: they happened, \
    but you didn't hear them. Never acknowledge them, in word or deed.";

/// What a bare `zz` says.
pub const GOODNIGHT: &str = "Goodnight.";

/// Told to the reply model when the user said goodnight with `zz`.
pub const GOODNIGHT_HINT: &str = "They are going to bed: this reply is your goodnight, short and \
    in character, and you fall asleep after it. A `zz` mark at the start of their message said \
    so: it is not something they said, so never mention it.";

/// Splits a leading `zz` off a message: the user is going to bed, and what
/// follows is their goodnight. A bare `zz` says [`GOODNIGHT`]. Any case, since
/// phone keyboards capitalize it; glued to a word (`zzz`, `zzzap`) it is just
/// text.
pub fn goodnight_marker(message: &str) -> (bool, &str) {
    let text = message.trim_start();
    let rest = text
        .get(..2)
        .filter(|z| z.eq_ignore_ascii_case("zz"))
        .map(|_| &text[2..]);
    match rest {
        Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
            let rest = rest.trim();
            (true, if rest.is_empty() { GOODNIGHT } else { rest })
        }
        _ => (false, message),
    }
}

/// Splits a leading `~~` off a message: the companion falls asleep into a
/// dream that starts with what follows. A bare `~~` drops it somewhere. `~~`
/// glued to a word is Markdown strikethrough, not a marker.
pub fn dream_marker(message: &str) -> (bool, &str) {
    match message.trim_start().strip_prefix("~~") {
        Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
            let rest = rest.trim();
            (true, if rest.is_empty() { DREAM_OPENING } else { rest })
        }
        _ => (false, message),
    }
}

/// Splits a leading `<<` off a dream message: an echo, a scene from the past.
/// It needs a scene after it; alone, or glued to a word, it is just text.
pub fn echo_marker(message: &str) -> (bool, &str) {
    match message.trim_start().strip_prefix("<<") {
        Some(rest) if rest.starts_with(char::is_whitespace) && !rest.trim().is_empty() => {
            (true, rest.trim())
        }
        _ => (false, message),
    }
}

/// Most paragraphs a `>>N` message can ask for: about what fits in a reply.
pub const MAX_STRETCH: u32 = 10;

/// Splits a leading `>>N` off a message: the user wants this one reply to set
/// the length rule aside and run to about N paragraphs (an action scene, a
/// story). Returns N and the message without the marker, which is what gets
/// recorded and recalled on; a bare `>>N` means go on. Anything else, a `>>`
/// mid-message included, is not a marker.
pub fn stretch(message: &str) -> (Option<u32>, &str) {
    let Some(rest) = message.trim_start().strip_prefix(">>") else {
        return (None, message);
    };
    let text = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    let n = rest[..rest.len() - text.len()].parse::<u32>();
    match n {
        Ok(n) if text.is_empty() || text.starts_with(char::is_whitespace) => {
            let text = text.trim();
            (
                Some(n.clamp(1, MAX_STRETCH)),
                if text.is_empty() { "Go on." } else { text },
            )
        }
        _ => (None, message),
    }
}

/// The line that lifts [`clanka_core::soul::TALK_HINT`]'s length rule for one
/// reply. It goes last, after the persona: models follow the latest instruction.
/// A host that keeps its own history shows the model the marker, so the line
/// says what it is.
pub fn stretch_hint(paragraphs: u32) -> String {
    let s = if paragraphs == 1 { "" } else { "s" };
    format!(
        "This reply only: set aside the length rule. Write it out in full, about {paragraphs} \
         paragraph{s}. A `>>` mark at the start of their message asked for this: it is not \
         something they said, so never mention it."
    )
}

/// A turn opened with its whole [`Context`], for a front end that builds the
/// model's prompt itself (`clanka chat`).
#[derive(Debug, Clone, PartialEq)]
pub enum Opened {
    /// See [`Begin::Doctor`].
    Doctor,
    /// See [`Begin::Frozen`].
    Frozen,
    /// See [`Begin::Awake`].
    Awake,
    Open(Box<Open>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Open {
    pub context: Context,
    /// The message as recorded: without its markers.
    pub message: String,
    /// Paragraphs a `>>N` marker asked for.
    pub stretch: Option<u32>,
    /// The message is an echo (`<<`).
    pub echo: bool,
    /// Something the model is shown was said out of earshot.
    pub unheard: bool,
    pub pending: Pending,
}

impl Open {
    /// The lines for this reply only, after everything else: the echo, the
    /// out-of-earshot rule and the length. Models follow the latest instruction.
    pub fn hints(&self) -> Vec<String> {
        let mut hints = Vec::new();
        if self.echo {
            hints.push(ECHO_HINT.to_owned());
        }
        if self.unheard {
            hints.push(UNHEARD_HINT.to_owned());
        }
        if self.pending.sleep {
            hints.push(GOODNIGHT_HINT.to_owned());
        }
        hints.extend(self.stretch.map(stretch_hint));
        hints
    }
}

/// The words a reply's `wake_up` call quotes as what wakes the companion, or
/// `None` when the reply didn't call it.
pub fn wake_call(calls: &[clanka_core::deps::ToolCall]) -> Option<String> {
    let call = calls.iter().find(|c| c.name == WAKE_UP)?;
    let args: serde_json::Value = serde_json::from_str(&call.arguments).unwrap_or_default();
    Some(args["because"].as_str().unwrap_or_default().to_owned())
}

/// Answers the companion's `wake_up` call in the turn `pending` holds, quoting
/// `because` as the words that wake it (see [`Companion::wake_up`]). When it
/// wakes, the dream ends with this turn's reply. Returns whether it wakes and
/// what to tell the model.
pub fn wake_up(
    c: &Companion,
    pending: &mut Pending,
    because: &str,
) -> Result<(bool, &'static str)> {
    let (wakes, note) = c.wake_up(because)?;
    pending.wake |= wakes;
    Ok((wakes, note))
}

/// Checks for the doctor token, assembles the turn's memory and records the
/// user's message. `first` is true on the host conversation's first turn, when
/// the model has seen nothing yet and gets the whole context; a clanka session
/// outlives host conversations, so the host must say.
///
/// A dream another handle left open (its front end exited or died) is lost
/// and dropped first. A dream this handle started goes on.
pub fn open(c: &mut Companion, message: &str, budget: usize, first: bool) -> Result<Opened> {
    let started = Instant::now();
    let opened = open_turn(c, message, budget, first);
    let open = match &opened {
        Ok(Opened::Open(open)) => Some(open),
        _ => None,
    };
    let count = |of: fn(&Open) -> usize| open.map_or(0, |open| of(open));
    let is = |of: fn(&Open) -> bool| open.is_some_and(|open| of(open));
    c.diagnose(&Event::TurnBegin {
        ms: ms_since(started),
        outcome: match &opened {
            Ok(Opened::Doctor) => "doctor",
            Ok(Opened::Frozen) => "frozen",
            Ok(Opened::Awake) => "awake",
            Ok(Opened::Open(_)) => "open",
            Err(_) => "error",
        },
        first,
        dream: is(|o| o.pending.dream),
        goodnight: is(|o| o.pending.sleep),
        echo: is(|o| o.echo),
        unheard: is(|o| o.unheard),
        stretch: open.and_then(|o| o.stretch),
        message_chars: count(|o| o.message.chars().count()),
        context_tokens: count(|o| o.context.tokens),
        memories: count(|o| o.context.memories.len()),
        hot_threads: count(|o| o.context.hot_threads.len()),
        omitted_turns: count(|o| o.context.omitted_turns),
        spent: is(|o| o.context.spent),
        error: opened.as_ref().err().map(Error::code),
    });
    opened
}

fn open_turn(c: &mut Companion, message: &str, budget: usize, first: bool) -> Result<Opened> {
    c.reload_settled_persona()?;
    if c.soul().is_doctor_token(message) {
        c.set_frozen(true)?;
        return Ok(Opened::Doctor);
    }
    if c.is_frozen()? {
        return Ok(Opened::Frozen);
    }
    c.drop_lost_dream()?;
    let (goodnight, message) = goodnight_marker(message);
    let (asleep, message) = dream_marker(message);
    let dream = match c.dreaming()? {
        Some(dream) => Some(dream),
        None if asleep => match c.start_dream() {
            Ok(dream) => Some(dream),
            Err(Error::Awake) => return Ok(Opened::Awake),
            Err(e) => return Err(e),
        },
        None => None,
    };
    let (echo, message) = match dream {
        Some(_) => echo_marker(message),
        None => (false, message),
    };
    let (stretch, message) = stretch(message);
    let ranking = c.ranking().clone();
    let mut context = c.assemble(message, budget, &ranking)?;
    if !first {
        c.keep_new(&mut context)?;
    }
    // In a dream, goodnight wakes the companion: the dream ends with this
    // reply, as when it fills the session.
    if goodnight && dream.is_some() {
        context.spent = true;
    }
    // A host replaying its history has the rule from the turn that needed it.
    let unheard = dream.is_none()
        && (has_unheard(message) || first && context.turns.iter().any(|t| has_unheard(&t.content)));
    let session = match dream {
        Some(dream) => dream,
        None => c.open_session()?,
    };
    let user_turn = if echo {
        c.record_echo(session, message)?
    } else {
        c.record_turn(session, Speaker::User, message)?
    };
    let recalled = context
        .hot_threads
        .iter()
        .chain(context.memories.iter().map(|r| &r.memory))
        .map(|m| m.id)
        .collect();
    let wake = dream.is_some() && context.spent;
    Ok(Opened::Open(Box::new(Open {
        context,
        message: message.to_owned(),
        stretch,
        echo,
        unheard,
        pending: Pending {
            session,
            user_turn,
            recalled,
            dream: dream.is_some(),
            wake,
            sleep: goodnight && dream.is_none(),
        },
    })))
}

/// [`open`] for a host that keeps the persona and the history itself: the
/// turn's memory comes back as one text to put in front of the model.
pub fn begin(c: &mut Companion, message: &str, budget: usize, first: bool) -> Result<Begin> {
    let open = match open(c, message, budget, first)? {
        Opened::Doctor => return Ok(Begin::Doctor),
        Opened::Frozen => return Ok(Begin::Frozen),
        Opened::Awake => return Ok(Begin::Awake),
        Opened::Open(open) => *open,
    };
    // Added after `keep_new`, so even a turn with nothing new carries them.
    let hints = open.hints();
    let mut context = render(open.context);
    for hint in hints {
        if !context.is_empty() {
            context.push_str("\n\n");
        }
        context.push_str(&hint);
    }
    Ok(Begin::Open {
        context,
        pending: open.pending,
    })
}

/// Records the reply, without its em dashes, and marks what was recalled.
/// Returns the (user, companion) turn IDs to reflect on.
pub fn end(c: &mut Companion, pending: Pending, reply: &str) -> Result<(i64, i64)> {
    let started = Instant::now();
    let said = clanka_core::tics::without_dashes(reply);
    let ended = c
        .record_turn(pending.session, Speaker::Companion, &said)
        .and_then(|turn| {
            c.mark_recalled(pending.session, &pending.recalled)?;
            Ok((pending.user_turn, turn))
        });
    c.diagnose(&Event::TurnEnd {
        ms: ms_since(started),
        reply_chars: said.chars().count(),
        dashes: said != reply,
        tics: Tics::of(&said),
        recalled: pending.recalled.len(),
        error: ended.as_ref().err().map(Error::code),
    });
    ended
}

/// What the reply model's own search of memory brings up (the recall tool a
/// host offers it), on the handle's ranking.
pub fn recall(c: &Companion, query: &str) -> Result<Vec<Recalled>> {
    let started = Instant::now();
    let found = c.recall(query, c.ranking());
    c.diagnose(&Event::Recall {
        ms: ms_since(started),
        found: found.as_ref().map_or(0, Vec::len),
        error: found.as_ref().err().map(Error::code),
    });
    found
}

/// The turn's context without the persona (the host holds it) or the session's
/// turns (the host keeps its own history). Empty when there is nothing new.
pub fn render(context: Context) -> String {
    Context {
        persona: String::new(),
        turns: Vec::new(),
        ..context
    }
    .render()
    .trim()
    .to_owned()
}
