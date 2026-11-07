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
    Ok(Begin::Open {
        context: render(context),
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
