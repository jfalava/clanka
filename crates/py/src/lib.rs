//! `_clanka`: a companion for Python host plugins (Hermes Agent first).
//!
//! The host supplies its own model as a Python callable, so memory is written
//! by the same model the companion chats with. The callable only returns text,
//! so the core asks it for JSON actions and runs them through the same guarded
//! handlers as native tool calls.
//!
//! Every method releases the GIL while the core works, and the callable takes
//! it back only for the model call. Like the MCP server, the companion has two
//! handles: one for the turn hooks and one for reflection and sleep, so a slow
//! reflection on the host's background worker never blocks the next turn.

use clanka::setup::{self, Paths, SharedEmbedder};
use clanka::turn::{self, Begin, Pending};
use clanka_core::deps::{Llm, LlmRequest, LlmResponse, Role};
use clanka_core::{Companion, Ranking};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

/// The host's model as `model(system, messages, temperature) -> str`, where
/// `messages` is a list of `{"role": "user" | "assistant", "content": str}`.
struct HostModel(Py<PyAny>);

impl Llm for HostModel {
    fn complete(&self, request: &LlmRequest) -> clanka_core::Result<LlmResponse> {
        let failed = |e: PyErr| clanka_core::Error::Llm(format!("host model: {e}"));
        Python::attach(|py| {
            let messages = request
                .messages
                .iter()
                .map(|m| {
                    let d = PyDict::new(py);
                    let role = match m.role {
                        Role::Assistant => "assistant",
                        // Text-only: tool results come back as user messages.
                        Role::User | Role::Tool => "user",
                    };
                    d.set_item("role", role)?;
                    d.set_item("content", &m.content)?;
                    Ok(d)
                })
                .collect::<PyResult<Vec<_>>>()
                .map_err(failed)?;
            let text: String = self
                .0
                .call1(py, (&request.system, messages, request.temperature))
                .and_then(|r| r.extract(py))
                .map_err(failed)?;
            Ok(LlmResponse {
                text,
                tool_calls: Vec::new(),
            })
        })
    }

    fn tool_calls(&self) -> bool {
        false
    }
}

fn err(e: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One companion, opened from its soul.md, with its key from the Secret
/// Service and the embedding model `clanka init` downloaded.
#[pyclass(frozen, name = "Companion", module = "_clanka")]
struct PyCompanion {
    chat: Arc<Mutex<Companion>>,
    writer: Arc<Mutex<Companion>>,
    pending: Mutex<Option<Pending>>,
    budget: usize,
    #[pyo3(get)]
    name: String,
    /// The persona text, without the frontmatter (which holds the doctor token).
    #[pyo3(get)]
    persona: String,
}

#[pymethods]
impl PyCompanion {
    #[new]
    #[pyo3(signature = (soul, model, budget = 6000))]
    fn new(py: Python<'_>, soul: PathBuf, model: Py<PyAny>, budget: usize) -> PyResult<Self> {
        // Both handles call the same host model.
        let models = [HostModel(model.clone_ref(py)), HostModel(model)];
        py.detach(|| {
            let paths = Paths::get().map_err(err)?;
            let embedder = SharedEmbedder::load(&paths).map_err(err)?;
            let [for_chat, for_writer] = models;
            let open = |llm: HostModel| {
                setup::open_with(&paths, &soul, embedder.clone(), Box::new(llm)).map_err(err)
            };
            let chat = open(for_chat)?;
            if let clanka_core::PersonaStatus::Changed { .. } = chat.persona_status() {
                return Err(err(format!(
                    "{} changed since its memories were written; run `clanka soul continue`, `update` or `fork` on it first",
                    soul.display()
                )));
            }
            let writer = open(for_writer)?;
            Ok(Self {
                name: chat.soul().traits.name.clone(),
                persona: chat.soul().body.trim().to_owned(),
                chat: Arc::new(Mutex::new(chat)),
                writer: Arc::new(Mutex::new(writer)),
                pending: Mutex::new(None),
                budget,
            })
        })
    }

    /// Before the model replies. Returns `("doctor", "")` when the message is
    /// the doctor token (memory is frozen, nothing recorded: don't send the
    /// message to the model), `("frozen", "")` while frozen, or
    /// `("open", context)` with the memory to give the model. On the host
    /// conversation's first turn (`first`) that is everything; later it is
    /// only what the model hasn't seen, and empty when nothing is new.
    #[pyo3(signature = (message, first = true))]
    fn begin_turn(
        &self,
        py: Python<'_>,
        message: &str,
        first: bool,
    ) -> PyResult<(&'static str, String)> {
        py.detach(|| {
            let begun =
                turn::begin(&mut lock(&self.chat), message, self.budget, first).map_err(err)?;
            Ok(match begun {
                Begin::Doctor => {
                    *lock(&self.pending) = None;
                    ("doctor", String::new())
                }
                Begin::Frozen => ("frozen", String::new()),
                Begin::Open { context, pending } => {
                    *lock(&self.pending) = Some(pending);
                    ("open", context)
                }
            })
        })
    }

    /// After the reply: records it and marks what was recalled. Fast. Returns
    /// the `(user, companion)` turn IDs to pass to `reflect`, or `None` when no
    /// turn was open (the doctor token, a frozen memory, or a vetoed turn).
    fn end_turn(&self, py: Python<'_>, reply: &str) -> PyResult<Option<(i64, i64)>> {
        py.detach(|| {
            let Some(pending) = lock(&self.pending).take() else {
                return Ok(None);
            };
            turn::end(&mut lock(&self.chat), pending, reply)
                .map(Some)
                .map_err(err)
        })
    }

    /// Reflects on one exchange with the host model, as the companion. Slow:
    /// call it off the hot path. Returns the count of actions that succeeded;
    /// for each that was refused (including a reply that wasn't valid JSON
    /// actions), `"<action>: <error>"`; and for each write that left facts
    /// out because they weren't the user's words, `"<action>: <result>"`.
    fn reflect(
        &self,
        py: Python<'_>,
        user_turn: i64,
        companion_turn: i64,
    ) -> PyResult<(usize, Vec<String>, Vec<String>)> {
        py.detach(|| {
            let r = lock(&self.writer)
                .reflect(user_turn, companion_turn)
                .map_err(err)?;
            let ok = r.calls.iter().filter(|o| o.ok).count();
            let describe =
                |o: &clanka_core::writer::ToolOutcome| format!("{}: {}", o.name, o.result);
            let refused = r.calls.iter().filter(|o| !o.ok).map(describe).collect();
            let left_out = r
                .calls
                .iter()
                .filter(|o| o.ok && o.result.contains(clanka_core::writer::LEFT_OUT))
                .map(describe)
                .collect();
            Ok((ok, refused, left_out))
        })
    }

    /// Memories for a deliberate attempt to remember, as interpretations.
    fn recall(&self, py: Python<'_>, query: &str) -> PyResult<Vec<String>> {
        py.detach(|| {
            let found = lock(&self.chat)
                .recall(query, &Ranking::default())
                .map_err(err)?;
            if let Some(p) = lock(&self.pending).as_mut() {
                p.recalled.extend(found.iter().map(|r| r.memory.id));
            }
            Ok(found.into_iter().map(|r| r.memory.interpretation).collect())
        })
    }

    /// Ends the conversation and consolidates it. Slow. Does nothing, and says
    /// so, when nothing was said since the companion last slept, so a host may
    /// call it at every conversation boundary.
    fn sleep(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| {
            let Some(session) = lock(&self.chat).session_in_progress().map_err(err)? else {
                return Ok("Already asleep: nothing was said since.".to_owned());
            };
            let report = lock(&self.writer).sleep(session).map_err(err)?;
            Ok(format!(
                "Asleep. The conversation is now a memory; {} memories shifted and {} unresolved \
                 things weigh more.",
                report.drifted.len(),
                report.escalated.len()
            ))
        })
    }

    fn is_frozen(&self, py: Python<'_>) -> PyResult<bool> {
        py.detach(|| lock(&self.chat).is_frozen().map_err(err))
    }
}

#[pymodule]
fn _clanka(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCompanion>()
}
