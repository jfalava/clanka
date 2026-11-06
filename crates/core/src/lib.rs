//! Companion memory engine.
//!
//! The core has no transport, no async runtime and no network code. Front ends
//! (CLI, PyO3, MCP, HTTP) sit on top and supply the outside world through the
//! traits in [`deps`]. Design: <https://ampcode.com/artifacts/companion-memory-design-4701080c-42d4-44c3-ac9e-95679849e8ae>

mod companion;
mod db;
pub mod deps;
mod error;
pub mod memory;
pub mod model;
pub mod soul;

pub use companion::{Companion, PersonaStatus};
pub use deps::Deps;
pub use error::{Error, Result};
pub use memory::{Memory, MemoryDraft};
