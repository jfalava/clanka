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

use clanka::diagnostics::{Host, Log, error_code};
use clanka::setup::{self, Config, Paths, SharedEmbedder};
use clanka::turn::{self, Begin, Pending};
use clanka_core::Companion;
use clanka_core::deps::{Llm, LlmRequest, LlmResponse, Role};
use clanka_core::diagnostics::{Event, PLUGIN_EVENTS, Recorder};
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

/// A Python error with the message, and the kind of error as its `code` (the
/// word diagnostics keep): the plugin tells the user the kind and logs the
/// message, which can name files on this machine.
fn err(e: impl Into<anyhow::Error>) -> PyErr {
    let e = e.into();
    let error = PyRuntimeError::new_err(e.to_string());
    Python::attach(|py| {
        // Without the code the plugin says `other`.
        let _ = error.value(py).setattr("code", error_code(&e));
    });
    error
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
            let chat = setup::open_to_talk(&paths, &soul, embedder.clone(), Box::new(for_chat))
                .map_err(err)?;
            let writer =
                setup::open_with(&paths, &soul, embedder, Box::new(for_writer)).map_err(err)?;
            Ok(Self {
                name: chat.soul().traits.name.clone(),
                persona: chat.soul().persona(),
                chat: Arc::new(Mutex::new(chat)),
                writer: Arc::new(Mutex::new(writer)),
                pending: Mutex::new(None),
                budget,
            })
        })
    }

    /// Before the model replies. Returns `("doctor", "")` when the message is
    /// the doctor token (memory is frozen, nothing recorded: don't send the
    /// message to the model), `("frozen", "")` while frozen, `("awake", why)`
    /// when the message asked for a dream (`~~`) mid-conversation (nothing
    /// recorded; show `why` instead of a reply), `("open", context)` with
    /// the memory to give the model, or `("goodnight", context)` for the same
    /// when the user said goodnight with `zz` (the companion sleeps after this
    /// reply: see `end_turn`). On the host conversation's first turn
    /// (`first`) that is everything; later it is only what the model hasn't
    /// seen, and empty when nothing is new.
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
                Begin::Awake => ("awake", turn::AWAKE.to_owned()),
                Begin::Open { context, pending } => {
                    let kind = if pending.sleep { "goodnight" } else { "open" };
                    *lock(&self.pending) = Some(pending);
                    (kind, context)
                }
            })
        })
    }

    /// After the reply: records it and marks what was recalled. Fast. Returns
    /// the `(user, companion)` turn IDs to pass to `reflect` and whether the
    /// companion sleeps after this reply (the user said goodnight with `zz`,
    /// or the dream ends: it filled the session, `wake_up` woke the companion,
    /// or `zz` did; call `sleep`, which in a dream wakes it), or `None` when
    /// no turn was open (the doctor token, a frozen memory, or a vetoed turn).
    /// `reflect` on a dream's turns does nothing.
    fn end_turn(&self, py: Python<'_>, reply: &str) -> PyResult<Option<(i64, i64, bool)>> {
        py.detach(|| {
            let Some(pending) = lock(&self.pending).take() else {
                return Ok(None);
            };
            let sleep = pending.wake || pending.sleep;
            let (user, companion) =
                turn::end(&mut lock(&self.chat), pending, reply).map_err(err)?;
            Ok(Some((user, companion, sleep)))
        })
    }

    /// Answers the reply model's `wake_up` call, which quotes `because` as the
    /// words of what happened that wake the companion. Returns whether it
    /// wakes and the answer to give the model. When it wakes, `end_turn` says
    /// the dream ends with this reply.
    fn wake_up(&self, py: Python<'_>, because: &str) -> PyResult<(bool, &'static str)> {
        py.detach(|| {
            let mut pending = lock(&self.pending);
            let Some(p) = pending.as_mut() else {
                return Ok((false, "No turn is open: call wake_up before your reply."));
            };
            turn::wake_up(&lock(&self.chat), p, because).map_err(err)
        })
    }

    /// Whether the companion is in a dream.
    fn dreaming(&self, py: Python<'_>) -> PyResult<bool> {
        py.detach(|| Ok(lock(&self.chat).dreaming().map_err(err)?.is_some()))
    }

    /// Drops the open dream, if any: it ends with nothing kept. For a host
    /// conversation that ended before the companion woke. Returns whether
    /// there was one.
    fn drop_dream(&self, py: Python<'_>) -> PyResult<bool> {
        py.detach(|| Ok(lock(&self.chat).drop_dream().map_err(err)?.is_some()))
    }

    /// Reflects on one exchange with the host model, as the companion. Slow:
    /// call it off the hot path. Returns the count of actions that succeeded;
    /// for each that was refused (including a reply that wasn't valid JSON
    /// actions), `"<action>: <error>"`; and for each write that left facts
    /// out because they weren't the user's words, `"<action>: <result>"`; and
    /// the count of earlier exchanges whose failed reflection was made up for
    /// first.
    fn reflect(
        &self,
        py: Python<'_>,
        user_turn: i64,
        companion_turn: i64,
    ) -> PyResult<(usize, Vec<String>, Vec<String>, usize)> {
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
            Ok((ok, refused, left_out, r.caught_up))
        })
    }

    /// Memories for a deliberate attempt to remember, as interpretations.
    fn recall(&self, py: Python<'_>, query: &str) -> PyResult<Vec<String>> {
        py.detach(|| {
            let found = turn::recall(&lock(&self.chat), query).map_err(err)?;
            if let Some(p) = lock(&self.pending).as_mut() {
                p.recalled.extend(found.iter().map(|r| r.memory.id));
            }
            Ok(found.into_iter().map(|r| r.memory.interpretation).collect())
        })
    }

    /// Ends the conversation and consolidates it. Slow. Does nothing, and says
    /// so, when nothing was said since the companion last slept, so a host may
    /// call it at every conversation boundary. In a dream, the companion wakes
    /// instead, and what stays of the dream is decided.
    fn sleep(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| {
            let dream = {
                let mut chat = lock(&self.chat);
                chat.drop_lost_dream().map_err(err)?;
                chat.dreaming().map_err(err)?
            };
            if let Some(dream) = dream {
                let report = lock(&self.writer).wake(dream).map_err(err)?;
                return Ok(format!(
                    "Awake. {} fragments linger, {} memories shifted{}.",
                    report.fragments.len(),
                    report.drifted.len(),
                    if report.seared.is_some() {
                        ", and one thing an echo showed stays seared"
                    } else {
                        ""
                    }
                ));
            }
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

/// The `wake_up` tool for the host to register: its description and its
/// parameters' JSON schema.
#[pyfunction]
fn wake_up_tool() -> (String, String) {
    let spec = clanka_core::dream::wake_up_tool();
    (spec.description, spec.parameters)
}

/// Notes something the plugin saw that the core can't, in the diagnostics
/// log: one of [`PLUGIN_EVENTS`], and never anything else, so nothing said can
/// be passed in. `soul` is the soul.md of the companion it happened to, which
/// need not be open. Does nothing when the log is off or the settings don't
/// load.
#[pyfunction]
#[pyo3(signature = (what, soul = None))]
fn note(py: Python<'_>, what: &str, soul: Option<PathBuf>) -> PyResult<()> {
    let Some(what) = PLUGIN_EVENTS.iter().find(|known| **known == what) else {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "not a plugin event; one of {PLUGIN_EVENTS:?}"
        )));
    };
    py.detach(|| {
        if let Ok(paths) = Paths::get()
            && let Ok(config) = Config::load(&paths)
        {
            let companion = soul.as_deref().and_then(setup::companion_id);
            Log::open(&paths, &config).record(companion, &Event::Plugin { what });
        }
    });
    Ok(())
}

/// Sends the diagnostics events not sent yet to the endpoint config.toml
/// names in `[diagnostics.upload]`, and returns how many went. 0, with
/// nothing sent, without that setting. For the end of a conversation.
#[pyfunction]
fn send_diagnostics(py: Python<'_>) -> PyResult<usize> {
    py.detach(|| {
        let paths = Paths::get().map_err(err)?;
        let config = Config::load(&paths).map_err(err)?;
        let sent = clanka::diagnostics::send_after_conversation(&paths, &config)
            .map_err(|e| err(anyhow::anyhow!("{e:#}")))?;
        Ok(sent.events)
    })
}

/// A reply without its em dashes, the way `end_turn` records it.
#[pyfunction]
fn without_dashes(reply: &str) -> String {
    clanka_core::tics::without_dashes(reply)
}

#[pymodule]
fn _clanka(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // The only front end this module is loaded by.
    clanka::diagnostics::start(Host::Hermes);
    m.add_class::<PyCompanion>()?;
    m.add_function(wrap_pyfunction!(note, m)?)?;
    m.add_function(wrap_pyfunction!(send_diagnostics, m)?)?;
    m.add_function(wrap_pyfunction!(without_dashes, m)?)?;
    m.add_function(wrap_pyfunction!(wake_up_tool, m)?)
}
