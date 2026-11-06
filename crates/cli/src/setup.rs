//! Where things live and how a companion handle is put together: the key from
//! the Secret Service, the local embedding model and the writer model.

use crate::llm::{NoWriter, OpenAiWriter};
use anyhow::{Context, Result, bail};
use clanka_core::deps::{Embedder, EmbeddingModel, FastEmbedder, KeySource, Rng, SystemClock};
use clanka_core::{Companion, Deps};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// Secret Service entries are `clanka` / `<companion id>`.
const KEYRING_SERVICE: &str = "clanka";

/// Conversations are in English, so the small English MiniLM (see the design's
/// embedding benchmark). Stored with every vector: changing it means a re-embed.
pub const MODEL_ID: &str = "all-MiniLM-L6-v2-q";
pub const MODEL: EmbeddingModel = EmbeddingModel::AllMiniLML6V2Q;
/// The files fastembed uses for `MODEL`, as (local name, path in the repo).
pub const MODEL_REPO: &str = "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main";
pub const MODEL_FILES: [(&str, &str); 5] = [
    ("model.onnx", "onnx/model_quantized.onnx"),
    ("tokenizer.json", "tokenizer.json"),
    ("config.json", "config.json"),
    ("special_tokens_map.json", "special_tokens_map.json"),
    ("tokenizer_config.json", "tokenizer_config.json"),
];

pub struct Paths {
    pub config: PathBuf,
    pub souls: PathBuf,
    pub data: PathBuf,
    pub models: PathBuf,
}

impl Paths {
    pub fn get() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "clanka")
            .context("no home directory to put clanka's files in")?;
        let config = dirs.config_dir().to_owned();
        let data = dirs.data_dir().to_owned();
        Ok(Self {
            souls: config.join("souls"),
            models: data.join("models").join(MODEL_ID),
            config,
            data,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }
}

/// `~/.config/clanka/config.toml`. Every part is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub writer: Option<WriterConfig>,
}

/// The model the writer, sleep and the doctor call: any OpenAI-compatible
/// chat completions endpoint with tool calls.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterConfig {
    /// Up to and including `/v1`, e.g. `https://api.example.com/v1`.
    pub base_url: String,
    pub model: String,
    /// Environment variable holding the API key.
    #[serde(default = "default_key_env")]
    pub api_key_env: String,
}

fn default_key_env() -> String {
    "CLANKA_WRITER_API_KEY".into()
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.config_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("reading {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

pub struct SecretServiceKeys;

impl SecretServiceKeys {
    fn entry(companion: Uuid) -> Result<keyring_core::Entry> {
        static STORE: std::sync::Once = std::sync::Once::new();
        let mut failed = None;
        STORE.call_once(|| match zbus_secret_service_keyring_store::Store::new() {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => failed = Some(e),
        });
        if let Some(e) = failed {
            bail!("Secret Service unavailable: {e}");
        }
        Ok(keyring_core::Entry::new(
            KEYRING_SERVICE,
            &companion.to_string(),
        )?)
    }

    pub fn store(companion: Uuid, key: &str) -> Result<()> {
        Self::entry(companion)?
            .set_password(key)
            .context("storing the database key in the Secret Service")
    }
}

impl KeySource for SecretServiceKeys {
    fn key(&self, companion: Uuid) -> clanka_core::Result<String> {
        let err = |e: String| clanka_core::Error::Key(e);
        Self::entry(companion)
            .map_err(|e| err(e.to_string()))?
            .get_password()
            .map_err(|e| err(format!("no key for {companion} in the Secret Service: {e}")))
    }
}

pub struct OsRng;

impl Rng for OsRng {
    fn next_seed(&self) -> u64 {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).expect("the OS has no randomness");
        u64::from_le_bytes(bytes)
    }
}

/// Random bytes as lowercase hex, `bytes * 2` characters long.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the OS has no randomness");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// One loaded embedding model shared by several handles, so two handles to
/// the same companion don't load it twice.
#[derive(Clone)]
pub struct SharedEmbedder(Arc<FastEmbedder>);

impl SharedEmbedder {
    pub fn load(paths: &Paths) -> Result<Self> {
        if !paths.models.join("model.onnx").exists() {
            bail!(
                "embedding model missing in {}; run `clanka init` to download it",
                paths.models.display()
            );
        }
        Ok(Self(Arc::new(FastEmbedder::from_dir(
            MODEL,
            MODEL_ID,
            &paths.models,
        )?)))
    }
}

impl Embedder for SharedEmbedder {
    fn model_id(&self) -> &str {
        self.0.model_id()
    }
    fn dimensions(&self) -> usize {
        self.0.dimensions()
    }
    fn embed(&self, texts: &[&str]) -> clanka_core::Result<Vec<Vec<f32>>> {
        self.0.embed(texts)
    }
}

/// Opens a companion. With a writer config, its model backs the writer, sleep
/// and the doctor; without one, those fail with a message saying how to set it.
pub fn open(
    paths: &Paths,
    soul: &Path,
    embedder: SharedEmbedder,
    writer: Option<&WriterConfig>,
) -> Result<Companion> {
    std::fs::create_dir_all(&paths.data)?;
    let llm: Box<dyn clanka_core::deps::Llm> = match writer {
        Some(w) => Box::new(OpenAiWriter::new(w.clone())),
        None => Box::new(NoWriter),
    };
    let deps = Deps {
        llm,
        embedder: Box::new(embedder),
        clock: Box::new(SystemClock),
        rng: Box::new(OsRng),
        keys: Box::new(SecretServiceKeys),
    };
    Ok(Companion::open(&paths.data, soul, deps)?)
}
