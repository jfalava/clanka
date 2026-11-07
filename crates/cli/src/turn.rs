//! One exchange as a host sees it: `begin` before the model replies, `end`
//! after. Shared by every front end where the host owns the conversation (MCP,
//! the Hermes extension), so the doctor token and freeze behave the same in all.

use clanka_core::model::Speaker;
use clanka_core::{Companion, Context, Ranking, Result};

/// The turn `begin` opened and `end` closes.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub session: i64,
    pub user_turn: i64,
    pub recalled: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Begin {
    /// The message was the doctor token: memory is now frozen and nothing was
    /// recorded. The host should not pass the message to the model.
    Doctor,
    /// Memory is frozen by the doctor: nothing is recorded or recalled.
    Frozen,
    /// The turn is recorded; `context` is the memory to give the model.
    Open { context: String, pending: Pending },
}

/// Checks for the doctor token, assembles the turn's memory and records the
/// user's message.
pub fn begin(c: &mut Companion, message: &str, budget: usize) -> Result<Begin> {
    if c.soul().is_doctor_token(message) {
        c.set_frozen(true)?;
        return Ok(Begin::Doctor);
    }
    if c.is_frozen()? {
        return Ok(Begin::Frozen);
    }
    let context = c.assemble(message, budget, &Ranking::default())?;
    let session = c.open_session()?;
    let user_turn = c.record_turn(session, Speaker::User, message)?;
    let recalled = context.memories.iter().map(|r| r.memory.id).collect();
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
/// turns (the host keeps its own history).
pub fn render(context: Context) -> String {
    let text = Context {
        persona: String::new(),
        turns: Vec::new(),
        ..context
    }
    .render();
    let text = text.trim();
    if text.is_empty() {
        "Nothing comes to mind; this is all new to you.".into()
    } else {
        text.into()
    }
}
