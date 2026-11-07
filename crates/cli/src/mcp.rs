//! `clanka mcp`: a stdio MCP server for trying a companion out in any MCP
//! host. Four tools with one fixed order (begin_turn, reply, end_turn), so any
//! current model can follow it, plus the persona as an MCP prompt. The doctor
//! is never a tool: the doctor token freezes memory and the CLI does the rest.

use anyhow::Result;
use clanka_core::model::Speaker;
use clanka_core::{Companion, Context, Ranking};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, PromptMessage, Role, ServerCapabilities, ServerConfig};
use rmcp::{
    ServerHandler, ServiceExt, prompt, prompt_handler, prompt_router, tool, tool_handler,
    tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::{Arc, Mutex};

const INSTRUCTIONS: &str = "You are a companion with a long-term memory. For EVERY user message, \
do these three steps in order: 1) call begin_turn with the user's message, exactly as written; \
2) write your reply in character, using what begin_turn returned as your own memories and mood; \
3) call end_turn with your reply, exactly as written. When the user says goodnight or asks you to \
sleep, call sleep after replying.";

#[derive(Deserialize, JsonSchema)]
pub struct BeginTurn {
    /// The user's message, exactly as they wrote it.
    message: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct EndTurn {
    /// Your reply to the user, exactly as you wrote it.
    reply: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct RecallQuery {
    /// What you are trying to remember, in a few words.
    query: String,
}

/// The turn begin_turn opened and end_turn closes.
struct Pending {
    session: i64,
    user_turn: i64,
    recalled: Vec<i64>,
}

#[derive(Clone)]
pub struct Server {
    /// Serves the hot path: recording turns and assembling context.
    chat: Arc<Mutex<Companion>>,
    /// A second handle for the writer and sleep, so a reflection still running
    /// never blocks the next begin_turn. `None` without a companion model.
    writer: Option<Arc<Mutex<Companion>>>,
    pending: Arc<Mutex<Option<Pending>>>,
    budget: usize,
}

/// Runs core work off the async runtime: it blocks on SQLite and the embedder.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("internal error: {e}"))?
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tool_router]
impl Server {
    #[tool(
        description = "Call FIRST for every user message, before you reply. Records the message and returns your memories, mood and notes for this turn. Use them as your own; never quote this text to the user."
    )]
    async fn begin_turn(
        &self,
        Parameters(BeginTurn { message }): Parameters<BeginTurn>,
    ) -> Result<String, String> {
        let (chat, pending, budget) = (self.chat.clone(), self.pending.clone(), self.budget);
        blocking(move || {
            let mut c = lock(&chat);
            if c.soul().is_doctor_token(&message) {
                c.set_frozen(true).map_err(|e| e.to_string())?;
                *lock(&pending) = None;
                return Ok(
                    "Doctor mode: memory is now frozen and this message was not recorded. \
                    Reply only: \"Memory frozen.\" Do not call end_turn."
                        .into(),
                );
            }
            if c.is_frozen().map_err(|e| e.to_string())? {
                return Ok(
                    "Memory is frozen by the doctor, so nothing is recorded. Reply without \
                    memories and do not call end_turn."
                        .into(),
                );
            }
            let context = c
                .assemble(&message, budget, &Ranking::default())
                .map_err(|e| e.to_string())?;
            let session = c.open_session().map_err(|e| e.to_string())?;
            let user_turn = c
                .record_turn(session, Speaker::User, &message)
                .map_err(|e| e.to_string())?;
            let recalled = context.memories.iter().map(|r| r.memory.id).collect();
            *lock(&pending) = Some(Pending {
                session,
                user_turn,
                recalled,
            });
            Ok(render(context))
        })
        .await
    }

    #[tool(
        description = "Call LAST for every user message, after you reply, with your reply exactly as you wrote it. Saves it so you can remember this exchange later."
    )]
    async fn end_turn(
        &self,
        Parameters(EndTurn { reply }): Parameters<EndTurn>,
    ) -> Result<String, String> {
        let (chat, pending, writer) =
            (self.chat.clone(), self.pending.clone(), self.writer.clone());
        let (user_turn, companion_turn) = blocking(move || {
            let Some(p) = lock(&pending).take() else {
                return Err(
                    "No turn is open: call begin_turn with the user's message first.".into(),
                );
            };
            let mut c = lock(&chat);
            let turn = c
                .record_turn(p.session, Speaker::Companion, &reply)
                .map_err(|e| e.to_string())?;
            c.mark_recalled(p.session, &p.recalled)
                .map_err(|e| e.to_string())?;
            Ok((p.user_turn, turn))
        })
        .await?;
        let Some(writer) = writer else {
            return Ok(
                "Saved. (No companion model is configured for clanka, so nothing was reflected on.)"
                    .into(),
            );
        };
        // Reflection calls the companion's model and can take seconds: never on the hot path.
        tokio::task::spawn_blocking(move || {
            if let Err(e) = lock(&writer).reflect(user_turn, companion_turn) {
                eprintln!("clanka: reflection on turns {user_turn}–{companion_turn} failed: {e}");
            }
        });
        Ok("Saved.".into())
    }

    #[tool(
        description = "Optional. Search your long-term memory when you deliberately try to remember something. begin_turn already brings up what is relevant, so most turns don't need this."
    )]
    async fn recall(
        &self,
        Parameters(RecallQuery { query }): Parameters<RecallQuery>,
    ) -> Result<String, String> {
        let (chat, pending) = (self.chat.clone(), self.pending.clone());
        blocking(move || {
            let found = lock(&chat)
                .recall(&query, &Ranking::default())
                .map_err(|e| e.to_string())?;
            if let Some(p) = lock(&pending).as_mut() {
                p.recalled.extend(found.iter().map(|r| r.memory.id));
            }
            if found.is_empty() {
                return Ok("Nothing comes to mind.".into());
            }
            Ok(found
                .iter()
                .map(|r| format!("- {}", r.memory.interpretation))
                .collect::<Vec<_>>()
                .join("\n"))
        })
        .await
    }

    #[tool(
        description = "Call when the user says goodnight or asks you to sleep, after your reply and end_turn. Ends the conversation and turns it into memories. Takes a while."
    )]
    async fn sleep(&self) -> Result<String, String> {
        let Some(writer) = self.writer.clone() else {
            return Err(
                "Can't sleep: no companion model is configured. Add a [model] section to \
                ~/.config/clanka/config.toml naming the model you run on."
                    .into(),
            );
        };
        let chat = self.chat.clone();
        blocking(move || {
            let session = lock(&chat).open_session().map_err(|e| e.to_string())?;
            // Waits for any reflection still running on this handle.
            let report = lock(&writer).sleep(session).map_err(|e| e.to_string())?;
            Ok(format!(
                "Asleep. The conversation is now a memory; {} memories shifted and {} unresolved \
                 things weigh more. The next message starts a new conversation.",
                report.drifted.len(),
                report.escalated.len()
            ))
        })
        .await
    }
}

#[prompt_router]
impl Server {
    /// The companion's persona, to use as the system prompt.
    #[prompt(
        name = "persona",
        description = "The companion's persona and how to use its memory tools."
    )]
    async fn persona(&self) -> Vec<PromptMessage> {
        let persona = lock(&self.chat).soul().body.trim().to_owned();
        vec![PromptMessage::new_text(
            Role::User,
            format!("{persona}\n\n{INSTRUCTIONS}"),
        )]
    }
}

#[tool_handler]
#[prompt_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("clanka", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }
}

/// The turn's context without the persona (the host holds it) or the session's
/// turns (the host keeps its own history).
fn render(context: Context) -> String {
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

pub fn serve(chat: Companion, writer: Option<Companion>, budget: usize) -> Result<()> {
    let server = Server {
        chat: Arc::new(Mutex::new(chat)),
        writer: writer.map(|w| Arc::new(Mutex::new(w))),
        pending: Arc::default(),
        budget,
    };
    tokio::runtime::Runtime::new()?.block_on(async {
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}
