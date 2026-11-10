//! Everything the core needs from the outside world. Front ends implement these;
//! the core never knows which model, host or key store is behind them.

use crate::Result;
use uuid::Uuid;

/// The dependencies one [`crate::Companion`] handle uses.
pub struct Deps {
    /// The companion's own model; see [`Llm`].
    pub llm: Box<dyn Llm>,
    pub embedder: Box<dyn Embedder>,
    pub clock: Box<dyn Clock>,
    pub rng: Box<dyn Rng>,
    pub keys: Box<dyn KeySource>,
}

/// The companion's own model: the same model, provider and identity the
/// companion chats with, never a separate "writer model". The writer, sleep and
/// doctor re-interpretation are separate calls to it, made in the companion's
/// voice. A front end backs this with the host's model client when the host
/// exposes one (with tool calls), or with the one model it chats with itself.
/// Blocking: an async front end wraps the core, not the other way round.
pub trait Llm: Send + Sync {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse>;

    /// Like [`Llm::complete`], but hands the reply's text to `on_text` piece by
    /// piece as it arrives, so a terminal can show it being written. A model
    /// with nothing to stream hands over the whole reply at once.
    fn stream(&self, request: &LlmRequest, on_text: &mut dyn FnMut(&str)) -> Result<LlmResponse> {
        let response = self.complete(request)?;
        if !response.text.is_empty() {
            on_text(&response.text);
        }
        Ok(response)
    }

    /// Whether the model takes `tools` and answers with `tool_calls`. A host
    /// whose model API only returns text says no; the core then asks for the
    /// same calls as one JSON object of actions and runs them through the same
    /// guarded handlers (see `actions`).
    fn tool_calls(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub system: String,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<ToolSpec>,
    pub temperature: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmMessage {
    pub role: Role,
    pub content: String,
    /// Assistant messages: the tool calls the model made.
    pub tool_calls: Vec<ToolCall>,
    /// Tool messages: the call this result answers.
    pub tool_call_id: Option<String>,
}

impl LlmMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self::text(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text(Role::Assistant, content)
    }

    fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments, as a JSON string.
    pub parameters: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LlmResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The provider's call ID, echoed back with the result.
    pub id: String,
    pub name: String,
    /// Arguments as a JSON string.
    pub arguments: String,
}

/// Text embeddings. Built from model files already on disk; fetching models is
/// the front end's job.
pub trait Embedder: Send + Sync {
    /// Stable identifier stored with every vector, e.g. `all-MiniLM-L6-v2-q`.
    fn model_id(&self) -> &str;
    fn dimensions(&self) -> usize;
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// Wall-clock time. Only time-away mood and record timestamps use it; erosion
/// and burial follow volume, never the clock.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

/// Randomness for drift. Every draw's seed is stored so the doctor can replay it.
pub trait Rng: Send + Sync {
    fn next_seed(&self) -> u64;
}

/// The SQLCipher key for a companion's database, e.g. from the Secret Service.
pub trait KeySource: Send + Sync {
    fn key(&self, companion: Uuid) -> Result<String>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        since.as_millis() as i64
    }
}

#[cfg(feature = "fastembed")]
pub use self::fastembed_impl::{EmbeddingModel, FastEmbedder};

#[cfg(feature = "fastembed")]
mod fastembed_impl {
    use super::Embedder;
    use crate::{Error, Result};
    pub use fastembed::EmbeddingModel;
    use fastembed::{
        InitOptionsUserDefined, QuantizationMode, TextEmbedding, TokenizerFiles,
        UserDefinedEmbeddingModel,
    };
    use std::path::Path;
    use std::sync::Mutex;

    /// ONNX Runtime threads per embedding.
    const EMBED_THREADS: usize = 1;

    /// fastembed with an ONNX model loaded from a local directory holding
    /// `model.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`
    /// and `tokenizer_config.json`.
    pub struct FastEmbedder {
        model_id: String,
        dimensions: usize,
        /// Dynamically quantized models scale each batch to its own range, so
        /// they embed one text at a time to give every text the same vector
        /// alone or in a batch.
        one_at_a_time: bool,
        inner: Mutex<TextEmbedding>,
    }

    impl FastEmbedder {
        /// `model` is the fastembed model the files are for; its pooling and
        /// quantization come from fastembed's own table, so they match the
        /// model's training (MiniLM mean-pools; fastembed's fallback is CLS).
        pub fn from_dir(
            model: EmbeddingModel,
            model_id: impl Into<String>,
            dir: &Path,
        ) -> Result<Self> {
            let read = |name: &str| {
                let path = dir.join(name);
                std::fs::read(&path).map_err(|source| Error::Io { path, source })
            };
            let quantization = TextEmbedding::get_quantization_mode(&model);
            let mut user_model = UserDefinedEmbeddingModel::new(
                read("model.onnx")?,
                TokenizerFiles {
                    tokenizer_file: read("tokenizer.json")?,
                    config_file: read("config.json")?,
                    special_tokens_map_file: read("special_tokens_map.json")?,
                    tokenizer_config_file: read("tokenizer_config.json")?,
                },
            )
            .with_quantization(quantization);
            if let Some(pooling) = TextEmbedding::get_default_pooling_method(&model) {
                user_model = user_model.with_pooling(pooling);
            }
            // One text at a time on a small model: more threads than this only
            // burn CPU on synchronisation (fastembed's default is every core).
            let options = InitOptionsUserDefined::default().with_intra_threads(EMBED_THREADS);
            let mut inner = TextEmbedding::try_new_from_user_defined(user_model, options)
                .map_err(|e| Error::Embedder(e.to_string()))?;
            let dimensions = inner
                .embed(vec!["probe"], None)
                .map_err(|e| Error::Embedder(e.to_string()))?[0]
                .len();
            Ok(Self {
                model_id: model_id.into(),
                dimensions,
                one_at_a_time: quantization == QuantizationMode::Dynamic,
                inner: Mutex::new(inner),
            })
        }
    }

    impl Embedder for FastEmbedder {
        fn model_id(&self) -> &str {
            &self.model_id
        }

        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            let mut inner = self.inner.lock().expect("embedder lock poisoned");
            let mut embed = |batch: &[&str]| {
                inner
                    .embed(batch, None)
                    .map_err(|e| Error::Embedder(e.to_string()))
            };
            if !self.one_at_a_time {
                return embed(texts);
            }
            let mut out = Vec::with_capacity(texts.len());
            for text in texts {
                out.extend(embed(std::slice::from_ref(text))?);
            }
            Ok(out)
        }
    }
}
