//! Companion memory engine.
//!
//! The core has no transport, no async runtime and no network code. Front ends
//! (CLI, PyO3, MCP, HTTP) sit on top and supply the outside world through the
//! traits in [`deps`]. Design: <https://ampcode.com/artifacts/companion-memory-design-4701080c-42d4-44c3-ac9e-95679849e8ae>

mod actions;
mod companion;
pub mod context;
mod db;
pub mod deps;
pub mod doctor;
mod error;
pub mod memory;
pub mod model;
pub mod recall;
pub mod sleep;
pub mod soul;
pub mod state;
pub mod writer;

pub use companion::{Companion, PersonaStatus};
pub use context::{Context, Fatigue, TimeAway, Turn};
pub use deps::Deps;
pub use error::{Error, Result};
pub use memory::{Deleted, Memory, MemoryDraft};
pub use recall::{Ranking, Recalled};
pub use sleep::SleepReport;
pub use state::{Mood, SectionVersion};
pub use writer::{Reflection, ToolOutcome};
