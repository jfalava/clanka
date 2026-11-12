//! Where things live and how a companion handle is put together: the key from
//! the Secret Service or a key file, the local embedding model, the
//! companion's model and config.toml's tuning.

use crate::llm::{NoModel, OpenAiModel};
use anyhow::{Context, Result, bail};
use clanka_core::deps::{Embedder, EmbeddingModel, FastEmbedder, KeySource, Llm, Rng, SystemClock};
use clanka_core::{Companion, Deps, Ranking};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// Secret Service entries are `clanka` / `<companion id>`.
const KEYRING_SERVICE: &str = "clanka";

/// The local model that turns text into vectors for recall. Its ID is stored
/// with every vector: a companion whose vectors came from another model
/// refuses to open until `clanka doctor reembed` rebuilds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Embedding {
    /// The small English MiniLM (see the design's embedding benchmark).
    #[default]
    English,
    /// The multilingual MiniLM, for conversations in other languages: about
    /// ten times the download and slower per text.
    Multilingual,
}

impl Embedding {
    pub fn id(self) -> &'static str {
        match self {
            Self::English => "all-MiniLM-L6-v2-q",
            Self::Multilingual => "paraphrase-multilingual-MiniLM-L12-v2-q",
        }
    }

    fn model(self) -> EmbeddingModel {
        match self {
            Self::English => EmbeddingModel::AllMiniLML6V2Q,
            Self::Multilingual => EmbeddingModel::ParaphraseMLMiniLML12V2Q,
        }
    }

    /// Where the files are downloaded from: one fixed revision, so the files
    /// are the ones the checksums in [`Self::files`] are for.
    pub fn repo(self) -> &'static str {
        match self {
            Self::English => {
                "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/751bff37182d3f1213fa05d7196b954e230abad9"
            }
            Self::Multilingual => {
                "https://huggingface.co/Qdrant/paraphrase-multilingual-MiniLM-L12-v2-onnx-Q/resolve/faf4aa4225822f3bc6376869cb1164e8e3feedd0"
            }
        }
    }

    /// The files fastembed uses for the model, as (local name, path in the
    /// repo, SHA-256). A changed vector model would silently stop matching
    /// every stored vector, so a file that isn't the expected one is refused.
    pub fn files(self) -> [(&'static str, &'static str, &'static str); 5] {
        match self {
            Self::English => [
                (
                    "model.onnx",
                    "onnx/model_quantized.onnx",
                    "afdb6f1a0e45b715d0bb9b11772f032c399babd23bfc31fed1c170afc848bdb1",
                ),
                (
                    "tokenizer.json",
                    "tokenizer.json",
                    "da0e79933b9ed51798a3ae27893d3c5fa4a201126cef75586296df9b4d2c62a0",
                ),
                (
                    "config.json",
                    "config.json",
                    "7135149f7cffa1a573466c6e4d8423ed73b62fd2332c575bf738a0d033f70df7",
                ),
                (
                    "special_tokens_map.json",
                    "special_tokens_map.json",
                    "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3",
                ),
                (
                    "tokenizer_config.json",
                    "tokenizer_config.json",
                    "9261e7d79b44c8195c1cada2b453e55b00aeb81e907a6664974b4d7776172ab3",
                ),
            ],
            Self::Multilingual => [
                (
                    "model.onnx",
                    "model_optimized.onnx",
                    "634d0f66c29dc934c8fa72b8a4fe91dd4d420a22f1d82a241058d4316e659a99",
                ),
                (
                    "tokenizer.json",
                    "tokenizer.json",
                    "fa685fc160bbdbab64058d4fc91b60e62d207e8dc60b9af5c002c5ab946ded00",
                ),
                (
                    "config.json",
                    "config.json",
                    "c8ec081fdad2df991bf5abbf18418fec7a5cdaa421f60ffb060a30040b8c376f",
                ),
                (
                    "special_tokens_map.json",
                    "special_tokens_map.json",
                    "8c785abebea9ae3257b61681b4e6fd8365ceafde980c21970d001e834cf10835",
                ),
                (
                    "tokenizer_config.json",
                    "tokenizer_config.json",
                    "0666eebf692422757e1dddf3c9fb1ded73ba3dc726c5828671fc89e45bf3609f",
                ),
            ],
        }
    }

    /// Where the model's files live.
    pub fn dir(self, paths: &Paths) -> PathBuf {
        paths.models.join(self.id())
    }
}

pub struct Paths {
    pub config: PathBuf,
    pub souls: PathBuf,
    pub data: PathBuf,
    /// One directory per embedding model.
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
            models: data.join("models"),
            config,
            data,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// The soul.md of every companion set up here, sorted by path.
    pub fn soul_files(&self) -> Result<Vec<PathBuf>> {
        let entries = match std::fs::read_dir(&self.souls) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.souls.display())),
        };
        let mut souls = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "md") {
                souls.push(path);
            }
        }
        souls.sort();
        Ok(souls)
    }
}

/// `~/.config/clanka/config.toml`. Every part is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub model: Option<ModelConfig>,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    /// Recall's tuning: any of [`Ranking`]'s fields; the rest keep their defaults.
    #[serde(default)]
    pub recall: Ranking,
    #[serde(default)]
    pub keys: KeysConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    #[serde(default)]
    pub model: Embedding,
    /// Cosine similarity from which a new memory is refused as a copy of one
    /// already kept. Another model spreads similar texts differently.
    pub duplicate_similarity: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeysConfig {
    #[serde(default)]
    pub store: KeyStore,
}

/// Where a new companion's database key is put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyStore {
    /// The Secret Service (the login keychain on macOS).
    #[default]
    SecretService,
    /// A file only you can read, `keys/<id>.key` beside config.toml, for a
    /// machine with no Secret Service. Whoever can read your files can then
    /// read the database too: the key no longer waits on your login.
    File,
}

/// The model that powers the companion: one definition for every role.
/// `clanka chat` talks with it, and the writer, sleep and the doctor call it.
/// Under `clanka mcp` the host does the talking, so this must name the same
/// model the host is set to; clanka can't see or check the host's model.
/// Any OpenAI-compatible chat completions endpoint with tool calls.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// Up to and including `/v1`, e.g. `https://api.example.com/v1`.
    pub base_url: String,
    pub model: String,
    /// Environment variable holding the API key.
    #[serde(default = "default_key_env")]
    pub api_key_env: String,
}

fn default_key_env() -> String {
    "CLANKA_MODEL_API_KEY".into()
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

/// The Secret Service on Linux; macOS has none, so its login keychain there.
#[cfg(not(target_os = "macos"))]
fn os_store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

#[cfg(target_os = "macos")]
fn os_store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

/// Where database keys are kept: the Secret Service, or a key file when you
/// chose that in config.toml. A companion's key file wins when it exists,
/// whatever the setting, so a companion moved here with its key still opens.
pub struct Keys {
    dir: PathBuf,
    store: KeyStore,
}

impl Keys {
    pub fn new(paths: &Paths, config: &Config) -> Self {
        Self {
            dir: paths.config.join("keys"),
            store: config.keys.store,
        }
    }

    fn file(&self, companion: Uuid) -> PathBuf {
        self.dir.join(format!("{companion}.key"))
    }

    fn entry(companion: Uuid) -> Result<keyring_core::Entry> {
        static STORE: std::sync::Once = std::sync::Once::new();
        static FAILED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        STORE.call_once(|| match os_store() {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => {
                let _ = FAILED.set(e.to_string());
            }
        });
        if let Some(e) = FAILED.get() {
            bail!("Secret Service unavailable: {e}");
        }
        Ok(keyring_core::Entry::new(
            KEYRING_SERVICE,
            &companion.to_string(),
        )?)
    }

    /// The key from its file, if the companion has one.
    fn read_file(&self, companion: Uuid) -> Result<Option<String>> {
        let path = self.file(companion);
        let key = match std::fs::read_to_string(&path) {
            Ok(key) => key,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)?.permissions().mode();
            if mode & 0o077 != 0 {
                bail!(
                    "{} can be read by other users; run `chmod 600` on it",
                    path.display()
                );
            }
        }
        Ok(Some(key.trim().to_owned()))
    }

    pub fn store(&self, companion: Uuid, key: &str) -> Result<()> {
        match self.store {
            KeyStore::SecretService => Self::entry(companion)
                .and_then(|entry| Ok(entry.set_password(key)?))
                .context(
                    "storing the database key in the Secret Service; on a machine without one, \
                     put `[keys]` and `store = \"file\"` in config.toml",
                ),
            KeyStore::File => {
                let path = self.file(companion);
                write_private(&self.dir, &path, key)
                    .with_context(|| format!("writing {}", path.display()))
            }
        }
    }

    /// Drops a key `store` put away, for a companion that was never set up
    /// after all. Best effort: a key left behind opens nothing.
    pub fn forget(&self, companion: Uuid) {
        match self.store {
            KeyStore::SecretService => {
                if let Ok(entry) = Self::entry(companion) {
                    let _ = entry.delete_credential();
                }
            }
            KeyStore::File => {
                let _ = std::fs::remove_file(self.file(companion));
            }
        }
    }
}

/// Writes a file only its owner can read, in a directory only they can enter.
/// Replaces an existing file.
pub fn write_private(dir: &Path, path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut dir_options = std::fs::DirBuilder::new();
    dir_options.recursive(true);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        dir_options.mode(0o700);
        options.mode(0o600);
    }
    dir_options.create(dir)?;
    // Written whole and renamed: a key is never half there.
    let partial = path.with_extension("part");
    let _ = std::fs::remove_file(&partial);
    let mut file = options.open(&partial)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&partial, path)
}

impl KeySource for Keys {
    fn key(&self, companion: Uuid) -> clanka_core::Result<String> {
        let err = |e: String| clanka_core::Error::Key(e);
        if let Some(key) = self
            .read_file(companion)
            .map_err(|e| err(format!("{e:#}")))?
        {
            return Ok(key);
        }
        Self::entry(companion)
            .and_then(|entry| Ok(entry.get_password()?))
            .map_err(|e| {
                err(format!(
                    "no key for {companion} in the Secret Service ({e:#}) or in {}",
                    self.file(companion).display()
                ))
            })
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

/// The SHA-256 of a file's contents, as lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// One loaded embedding model shared by several handles, so two handles to
/// the same companion don't load it twice.
#[derive(Clone)]
pub struct SharedEmbedder(Arc<FastEmbedder>);

impl SharedEmbedder {
    /// Loads the embedding model config.toml names.
    pub fn load(paths: &Paths) -> Result<Self> {
        let embedding = Config::load(paths)?.embedding.model;
        let dir = embedding.dir(paths);
        // Every file, not only the model: an interrupted download leaves some.
        if let Some((missing, ..)) = embedding
            .files()
            .iter()
            .find(|(local, ..)| !dir.join(local).exists())
        {
            bail!(
                "embedding model file {} is missing; run `clanka embedding`",
                dir.join(missing).display()
            );
        }
        Ok(Self(Arc::new(FastEmbedder::from_dir(
            embedding.model(),
            embedding.id(),
            &dir,
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

/// Opens a companion backed by its model. Without one, the writer, sleep and
/// the doctor fail with a message saying how to set it.
pub fn open(
    paths: &Paths,
    soul: &Path,
    embedder: SharedEmbedder,
    model: Option<&ModelConfig>,
) -> Result<Companion> {
    let llm: Box<dyn Llm> = match model {
        Some(m) => Box::new(OpenAiModel::new(m.clone())),
        None => Box::new(NoModel),
    };
    open_with(paths, soul, embedder, llm)
}

/// Opens a companion on a model the host supplies, e.g. a plugin's handle on
/// the host's own active model.
pub fn open_with(
    paths: &Paths,
    soul: &Path,
    embedder: SharedEmbedder,
    llm: Box<dyn Llm>,
) -> Result<Companion> {
    let config = Config::load(paths)?;
    let mut companion = Companion::open(&paths.data, soul, deps(paths, &config, embedder, llm)?)?;
    // Plain messages, not core errors: a bad setting isn't a broken companion.
    let config_file = paths.config_file();
    let bad = |e: clanka_core::Error| anyhow::anyhow!("{}: {e}", config_file.display());
    companion.set_ranking(config.recall).map_err(bad)?;
    if let Some(similarity) = config.embedding.duplicate_similarity {
        companion
            .set_duplicate_similarity(similarity)
            .map_err(bad)?;
    }
    Ok(companion)
}

/// The doctor's re-embed: opens a companion whose vectors came from another
/// embedding model and rebuilds every one with `embedder`. `progress` hears how
/// many memories are embedded, of how many.
pub fn reembed(
    paths: &Paths,
    soul: &Path,
    embedder: SharedEmbedder,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Companion> {
    let deps = deps(paths, &Config::load(paths)?, embedder, Box::new(NoModel))?;
    Ok(Companion::reembed(&paths.data, soul, deps, progress)?)
}

fn deps(
    paths: &Paths,
    config: &Config,
    embedder: SharedEmbedder,
    llm: Box<dyn Llm>,
) -> Result<Deps> {
    std::fs::create_dir_all(&paths.data)?;
    Ok(Deps {
        llm,
        embedder: Box::new(embedder),
        clock: Box::new(SystemClock),
        rng: Box::new(OsRng),
        keys: Box::new(Keys::new(paths, config)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> Paths {
        Paths {
            config: root.join("config"),
            souls: root.join("config/souls"),
            data: root.join("data"),
            models: root.join("data/models"),
        }
    }

    #[test]
    fn config_reads_the_optional_sections() {
        let config: Config = toml::from_str(
            "[embedding]\nmodel = \"multilingual\"\nduplicate_similarity = 0.9\n\
             [recall]\nlimit = 3\n\
             [keys]\nstore = \"file\"\n",
        )
        .unwrap();
        assert_eq!(config.embedding.model, Embedding::Multilingual);
        assert_eq!(config.embedding.duplicate_similarity, Some(0.9));
        assert_eq!(config.recall.limit, 3);
        // A field the file leaves out keeps its default.
        assert_eq!(config.recall.seared_limit, Ranking::default().seared_limit);
        assert_eq!(config.keys.store, KeyStore::File);

        let empty: Config = toml::from_str("").unwrap();
        assert_eq!(empty.embedding.model, Embedding::English);
        assert_eq!(empty.keys.store, KeyStore::SecretService);
        // A misspelt setting is an error, not silently ignored.
        assert!(toml::from_str::<Config>("[recall]\nlimits = 3\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_key_file_is_private_and_refused_once_others_can_read_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let config: Config = toml::from_str("[keys]\nstore = \"file\"\n").unwrap();
        let keys = Keys::new(&paths, &config);
        let id = Uuid::from_u128(7);

        keys.store(id, "abc123").unwrap();
        let file = paths.config.join(format!("keys/{id}.key"));
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(keys.key(id).unwrap(), "abc123");

        // The file wins whatever config.toml says, so the Secret Service is never asked.
        let keyring = Keys::new(&paths, &Config::default());
        assert_eq!(keyring.key(id).unwrap(), "abc123");

        // Storing again replaces the key.
        keys.store(id, "def456").unwrap();
        assert_eq!(keys.key(id).unwrap(), "def456");

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = keys.key(id).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
    }
}
