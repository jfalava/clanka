//! One exchange as a host sees it: `begin` before the model replies, `end`
//! after. Shared by every front end where the host owns the conversation (MCP,
//! the Hermes extension), so the doctor token and freeze behave the same in all.
//!
//! Such a host replays everything it was given on every later turn, so a turn
//! carries only what the model hasn't seen this session: the first turn gets
//! the whole context, later ones only what changed and memories new tonight.

use clanka_core::model::Speaker;
use clanka_core::{Companion, Context, Ranking, Result};

/// The turn `begin` opened and `end` closes.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub session: i64,
    pub user_turn: i64,
    /// Memories and hot threads shown this turn.
    pub recalled: Vec<i64>,
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
pub fn stretch_hint(paragraphs: u32) -> String {
    let s = if paragraphs == 1 { "" } else { "s" };
    format!(
        "This reply only: set aside the length rule. Write it out in full, about {paragraphs} \
         paragraph{s}."
    )
}

/// Checks for the doctor token, assembles the turn's memory and records the
/// user's message. `first` is true on the host conversation's first turn, when
/// the model has seen nothing yet and gets the whole context; a clanka session
/// outlives host conversations, so the host must say.
pub fn begin(c: &mut Companion, message: &str, budget: usize, first: bool) -> Result<Begin> {
    if c.soul().is_doctor_token(message) {
        c.set_frozen(true)?;
        return Ok(Begin::Doctor);
    }
    if c.is_frozen()? {
        return Ok(Begin::Frozen);
    }
    let (stretch, message) = stretch(message);
    let mut context = c.assemble(message, budget, &Ranking::default())?;
    if !first {
        c.keep_new(&mut context)?;
    }
    let session = c.open_session()?;
    let user_turn = c.record_turn(session, Speaker::User, message)?;
    let recalled = context
        .hot_threads
        .iter()
        .chain(context.memories.iter().map(|r| &r.memory))
        .map(|m| m.id)
        .collect();
    let mut context = render(context);
    // Added after `keep_new`, so even a turn with nothing new carries it.
    if let Some(n) = stretch {
        if !context.is_empty() {
            context.push_str("\n\n");
        }
        context.push_str(&stretch_hint(n));
    }
    Ok(Begin::Open {
        context,
        pending: Pending {
            session,
            user_turn,
            recalled,
        },
    })
}

/// Records the reply and marks what was recalled. Returns the (user, companion)
/// turn IDs to reflect on.
pub fn end(c: &mut Companion, pending: Pending, reply: &str) -> Result<(i64, i64)> {
    let turn = c.record_turn(pending.session, Speaker::Companion, reply)?;
    c.mark_recalled(pending.session, &pending.recalled)?;
    Ok((pending.user_turn, turn))
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
