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
//! (ending the conversation first, if one is going on), `<<` shows the dreaming companion
//! an echo of the past, `>>N` asks for a reply of about N paragraphs.
//! Anywhere in a waking message, text between a pair of `||` is said out of
//! earshot.

use clanka_core::diagnostics::{Event, Tics, ms_since};
use clanka_core::dream::{ECHO_HINT, WAKE_UP, WAKING};
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
    /// The user said goodnight with `zz`: the night ends with this reply,
    /// with no reflection on it, and the front end consolidates it behind.
    pub sleep: bool,
    /// `~~` ended the conversation going on to start this dream: its night
    /// waits for the front end to consolidate it.
    pub closed: Option<i64>,
}

impl Pending {
    /// [`end`] leaves a night or a dream for the writer to consolidate
    /// ([`Companion::consolidate`]): a goodnight, a waking, or a dream that
    /// ended the conversation before it.
    pub fn consolidates(&self) -> bool {
        self.sleep || self.wake || self.closed.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Begin {
    /// The message was the doctor token: memory is now frozen and nothing was
    /// recorded. The host should not pass the message to the model.
    Doctor,
    /// Memory is frozen by the doctor: nothing is recorded or recalled.
    Frozen,
    /// The turn is recorded; `context` is the memory to give the model, empty
    /// when nothing is new since the last turn.
    Open { context: String, pending: Pending },
}

/// What a bare `~~` puts in the dream: somewhere to be.
pub const DREAM_OPENING: &str = "You are somewhere you almost know.";

/// Told to the reply model when the user wrote something out of the
/// companion's earshot. The model knows it, so the story holds together; the
/// companion never acts as if it heard.
pub const UNHEARD_HINT: &str = "Words between || were said out of your earshot: they happened, \
    but you didn't hear them. Never acknowledge them, in word or deed.";

/// Told to the reply model in a dream when the user woke the companion with
/// `zz`, after [`clanka_core::dream::WAKING_HINT`]. Without it the mark reads as
/// part of the scene, and a small model weighs calling `wake_up` instead of
/// waking. It never says "dream", like the rest of the dream's lines.
pub const WAKE_MARK_HINT: &str = "A `zz` mark at the start of their message woke you: it is not \
    something that happened, so never mention it. You wake with this reply; don't call wake_up.";

/// What a bare `zz` says.
pub const GOODNIGHT: &str = "Goodnight.";

/// Told to the reply model when the user said goodnight with `zz`. What follows
/// the mark can be a story beat rather than a goodnight, so the reply answers
/// that and ends where it ends: told "this reply is your goodnight", a model
/// played the beat and then left the scene to go to bed.
pub const GOODNIGHT_HINT: &str = "Their message ends the conversation: answer it short, in \
    character and in the moment, as its last beat, and you fall asleep after this reply. Say \
    goodnight or speak of bed only if their message does. A `zz` mark at the start of their \
    message ended it: it is not something they said, so never mention it.";

/// Splits a leading `zz` off a message: the conversation ends with this
/// exchange, and what follows is the user's last message. A bare `zz` says [`GOODNIGHT`]. Any case, since
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
/// dream that starts with what follows, with a space after `~~` or without. A
/// bare `~~` drops it somewhere. Glued to a word and closed later in the
/// message it is Markdown strikethrough, and `~~~` is a code fence: neither is
/// a marker.
pub fn dream_marker(message: &str) -> (bool, &str) {
    match message.trim_start().strip_prefix("~~") {
        Some(rest)
            if rest.is_empty()
                || rest.starts_with(char::is_whitespace)
                || !rest.starts_with('~') && !rest.contains("~~") =>
        {
            let rest = rest.trim();
            (true, if rest.is_empty() { DREAM_OPENING } else { rest })
        }
        _ => (false, message),
    }
}

/// Splits a leading `<<` off a dream message: an echo, a scene from the past.
/// It needs a scene after it, with a space after `<<` or without; alone it is
/// just text, and so is a glued `<<` closed by a later `>>` (`<<Hola>>`, typed
/// guillemets). A `>>N` right after it is still the length marker.
pub fn echo_marker(message: &str) -> (bool, &str) {
    match message.trim_start().strip_prefix("<<") {
        Some(rest)
            if !rest.trim().is_empty()
                && (rest.starts_with(char::is_whitespace)
                    || rest.find(">>").is_none_or(|i| i == 0)) =>
        {
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
/// recorded and recalled on; a bare `>>N` means go on, and `>> N` is the same
/// marker. Anything else, a `>>` mid-message or one without a number included,
/// is not a marker.
pub fn stretch(message: &str) -> (Option<u32>, &str) {
    let Some(rest) = message.trim_start().strip_prefix(">>") else {
        return (None, message);
    };
    let rest = rest.trim_start_matches([' ', '\t']);
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
    /// The user woke the companion from a dream with `zz`.
    pub woken: bool,
    /// The message before this one in the session got no reply.
    pub unanswered: bool,
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
        if self.woken {
            hints.push(WAKE_MARK_HINT.to_owned());
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
///
/// A dream that ends with this reply anyway wakes whatever the call quotes.
/// The model was told it wakes ([`clanka_core::dream::WAKING_HINT`]), and it
/// quotes that line:
/// an answer that it is still there would tell it the opposite.
pub fn wake_up(
    c: &Companion,
    pending: &mut Pending,
    because: &str,
) -> Result<(bool, &'static str)> {
    let ending = pending.wake;
    let answered = if ending {
        Ok((true, WAKING))
    } else {
        c.wake_up(because)
    };
    c.diagnose(&Event::WakeUp {
        wakes: answered.as_ref().is_ok_and(|(wakes, _)| *wakes),
        ending,
        unopened: false,
        quoted_words: because.split_whitespace().count(),
        error: answered.as_ref().err().map(Error::code),
    });
    let (wakes, note) = answered?;
    pending.wake |= wakes;
    Ok((wakes, note))
}

/// Notes a `wake_up` call made while no turn is open, which a front end that
/// holds the turn answers itself: there is nothing to wake from.
pub fn wake_up_without_turn(c: &Companion, because: &str) {
    c.diagnose(&Event::WakeUp {
        wakes: false,
        ending: false,
        unopened: true,
        quoted_words: because.split_whitespace().count(),
        error: None,
    });
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
            Ok(Opened::Open(_)) => "open",
            Err(_) => "error",
        },
        first,
        dream: is(|o| o.pending.dream),
        goodnight: is(|o| o.pending.sleep || o.woken),
        echo: is(|o| o.echo),
        unheard: is(|o| o.unheard),
        unanswered: is(|o| o.unanswered),
        stretch: open.and_then(|o| o.stretch),
        message_chars: count(|o| o.message.chars().count()),
        context_tokens: count(|o| o.context.tokens),
        conversation_tokens: count(|o| o.context.conversation_tokens),
        allowance: c.session_tokens(),
        fatigue: open.map_or("rested", |o| {
            clanka_core::context::fatigue(
                &c.soul().traits,
                o.context.conversation_tokens,
                o.context.allowance,
            )
            .word()
        }),
        memories: count(|o| o.context.memories.len()),
        hot_threads: count(|o| o.context.hot_threads.len()),
        omitted_turns: count(|o| o.context.omitted_turns),
        spent: is(|o| o.context.spent),
        error: opened.as_ref().err().map(Error::code),
    });
    opened
}

fn open_turn(c: &mut Companion, message: &str, budget: usize, first: bool) -> Result<Opened> {
    c.refresh()?;
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
    let (dream, closed) = match c.dreaming()? {
        Some(dream) => (Some(dream), None),
        None if asleep => {
            let (dream, closed) = c.start_dream()?;
            (Some(dream), closed)
        }
        None => (None, None),
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
    let unanswered = c.unanswered(session)?;
    let user_turn = if echo {
        c.record_echo(session, message)?
    } else {
        c.record_turn(session, Speaker::User, message)?
    };
    let recalled = context
        .told
        .iter()
        .chain(&context.hot_threads)
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
        woken: goodnight && dream.is_some(),
        unanswered,
        pending: Pending {
            session,
            user_turn,
            recalled,
            dream: dream.is_some(),
            wake,
            sleep: goodnight && dream.is_none(),
            closed,
        },
    })))
}

/// [`open`] for a host that keeps the persona and the history itself: the
/// turn's memory comes back as one text to put in front of the model.
pub fn begin(c: &mut Companion, message: &str, budget: usize, first: bool) -> Result<Begin> {
    let open = match open(c, message, budget, first)? {
        Opened::Doctor => return Ok(Begin::Doctor),
        Opened::Frozen => return Ok(Begin::Frozen),
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
/// A goodnight or a waking ends the night or the dream with it, so the next
/// message starts a new conversation at once; the front end consolidates it
/// behind ([`Pending::consolidates`]). Returns the (user, companion) turn IDs
/// to reflect on.
pub fn end(c: &mut Companion, pending: Pending, reply: &str) -> Result<(i64, i64)> {
    let started = Instant::now();
    let said = clanka_core::tics::without_dashes(reply);
    let ended = c
        .record_turn(pending.session, Speaker::Companion, &said)
        .and_then(|turn| {
            c.mark_recalled(pending.session, &pending.recalled)?;
            if pending.sleep || pending.wake {
                c.close_night(pending.session)?;
            }
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
