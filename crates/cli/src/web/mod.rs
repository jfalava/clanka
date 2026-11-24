//! `clanka web`: a page on this machine for looking through what companions
//! remember. The server only reads: every route is a GET, and nothing here
//! calls a write of the core. It listens on 127.0.0.1 alone, and the API
//! answers only requests that carry the token made for this run, because
//! what it serves is what was said to a companion.
//!
//! The page itself is `web/` in the repository, built ahead of time and put
//! into the binary by `build.rs`.

use anyhow::{Context, Result};
use axum::extract::{Path as Route, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clanka::setup::{self, Paths, SharedEmbedder};
use clanka_core::doctor::{FactIssue, WriteEntry};
use clanka_core::model::Section;
use clanka_core::soul::Soul;
use clanka_core::{
    Companion, Memory, MemoryVersion, Mood, PersonaStatus, Recalled, SectionVersion, SourceTurn,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use uuid::Uuid;

#[cfg(test)]
#[path = "../../tests/common/mod.rs"]
mod common;
#[cfg(test)]
mod tests;

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// The port `clanka web` listens on unless told another.
pub const PORT: u16 = 7333;

type Open = Box<dyn Fn(&Path) -> Result<Companion> + Send + Sync>;

/// What the server reads from: the companions set up here, each opened the
/// first time the page asks for it and kept open after.
pub struct Console {
    paths: Paths,
    open: Open,
    token: String,
    opened: Mutex<HashMap<Uuid, Arc<Mutex<Companion>>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Console {
    pub fn new(
        paths: Paths,
        token: String,
        open: impl Fn(&Path) -> Result<Companion> + Send + Sync + 'static,
    ) -> Self {
        Self {
            paths,
            open: Box::new(open),
            token,
            opened: Mutex::new(HashMap::new()),
        }
    }

    fn database(&self, id: Uuid) -> PathBuf {
        self.paths.data.join(format!("{id}.db"))
    }

    /// Every soul.md here, without opening a database.
    fn companions(&self) -> Result<Vec<Listed>> {
        let listed = self.paths.soul_files()?.into_iter().map(|path| {
            let soul = path.display().to_string();
            match Soul::load(&path) {
                Ok(loaded) => Listed {
                    soul,
                    id: Some(loaded.traits.id),
                    name: Some(loaded.traits.name),
                    db_bytes: std::fs::metadata(self.database(loaded.traits.id))
                        .ok()
                        .map(|meta| meta.len()),
                    error: None,
                },
                Err(e) => Listed {
                    soul,
                    id: None,
                    name: None,
                    db_bytes: None,
                    error: Some(e.to_string()),
                },
            }
        });
        Ok(listed.collect())
    }

    /// The handle to a companion, opened if this is the first time. One that
    /// was never opened has no database, and looking at it must not make one.
    fn companion(&self, id: Uuid) -> Result<Arc<Mutex<Companion>>, Failure> {
        if let Some(handle) = lock(&self.opened).get(&id) {
            return Ok(handle.clone());
        }
        let soul = self
            .paths
            .soul_files()
            .map_err(broken)?
            .into_iter()
            .find(|path| Soul::load(path).is_ok_and(|soul| soul.traits.id == id))
            .ok_or_else(|| Failure::Missing(format!("no companion {id} here")))?;
        if !self.database(id).exists() {
            return Err(Failure::Missing(format!(
                "{} has never been opened, so it has no memory yet",
                soul.display()
            )));
        }
        let companion = (self.open)(&soul).map_err(broken)?;
        Ok(lock(&self.opened)
            .entry(id)
            .or_insert_with(|| Arc::new(Mutex::new(companion)))
            .clone())
    }

    /// Runs `read` on a companion that has caught up with the disk.
    fn read<T>(
        &self,
        id: Uuid,
        read: impl FnOnce(&Companion) -> clanka_core::Result<T>,
    ) -> Result<T, Failure> {
        let handle = self.companion(id)?;
        let mut companion = lock(&handle);
        if let Err(e) = companion.refresh() {
            // Its database was replaced or its soul.md no longer loads: the
            // next request opens it again from what is there.
            lock(&self.opened).remove(&id);
            return Err(e.into());
        }
        Ok(read(&companion)?)
    }
}

/// A refusal, as the status and the `{"error": …}` the page shows.
#[derive(Debug)]
enum Failure {
    Unauthorized,
    Forbidden,
    Missing(String),
    Broken(String),
}

fn broken(error: impl std::fmt::Display) -> Failure {
    Failure::Broken(format!("{error:#}"))
}

impl From<clanka_core::Error> for Failure {
    fn from(error: clanka_core::Error) -> Self {
        match error {
            clanka_core::Error::NotFound(_) => Failure::Missing(error.to_string()),
            _ => Failure::Broken(error.to_string()),
        }
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Failure::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "This page has no token for this run of clanka web. Open the address it printed."
                    .into(),
            ),
            Failure::Forbidden => (
                StatusCode::FORBIDDEN,
                "clanka web answers only as 127.0.0.1 or localhost.".into(),
            ),
            Failure::Missing(error) => (StatusCode::NOT_FOUND, error),
            Failure::Broken(error) => (StatusCode::INTERNAL_SERVER_ERROR, error),
        };
        (status, Json(json!({ "error": error }))).into_response()
    }
}

#[derive(Serialize)]
struct Listed {
    soul: String,
    /// `None` when the soul.md does not load; `error` says why.
    id: Option<Uuid>,
    name: Option<String>,
    /// The size of its database, or `None` when it was never opened.
    db_bytes: Option<u64>,
    error: Option<String>,
}

#[derive(Serialize)]
struct Overview {
    id: Uuid,
    name: String,
    soul: String,
    frozen: bool,
    archived: bool,
    /// Whether the soul.md still holds the persona its memories were written under.
    persona_changed: bool,
    /// `dream`, `conversation` or `none`.
    session: &'static str,
    /// Nights that ended and still wait to be slept on.
    nights_waiting: usize,
    mood: Option<Mood>,
    /// The note the companion left itself for its next reply.
    note: Option<String>,
    sections: Vec<SectionHistory>,
}

#[derive(Serialize)]
struct SectionHistory {
    name: Section,
    /// Every version, oldest first; the last one is current.
    versions: Vec<SectionVersion>,
}

#[derive(Serialize)]
struct Detail {
    versions: Vec<MemoryVersion>,
    turns: Vec<SourceTurn>,
}

#[derive(Serialize)]
struct Found {
    found: Vec<Recalled>,
    limit: usize,
    min_relevance: f64,
    min_score: f64,
}

#[derive(Deserialize)]
struct RecallQuery {
    q: String,
}

#[derive(Deserialize)]
struct LogQuery {
    /// Milliseconds since the epoch; writes at or after it. Everything when absent.
    since: Option<i64>,
}

type Shared = State<Arc<Console>>;
type Answer<T> = Result<Json<T>, Failure>;

/// Core work off the async thread, as the core's design asks of an async front end.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Answer<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(broken)?
        .map(Json)
}

async fn companions(State(console): Shared) -> Answer<Vec<Listed>> {
    blocking(move || console.companions().map_err(broken)).await
}

fn overview_of(c: &Companion) -> clanka_core::Result<Overview> {
    let session = if c.dreaming()?.is_some() {
        "dream"
    } else if c.session_in_progress()?.is_some() {
        "conversation"
    } else {
        "none"
    };
    let mut sections = Vec::new();
    for name in [Section::AboutUser, Section::Relationship] {
        sections.push(SectionHistory {
            name,
            versions: c.section_history(name)?,
        });
    }
    let traits = &c.soul().traits;
    Ok(Overview {
        id: traits.id,
        name: traits.name.clone(),
        soul: c.soul_path().display().to_string(),
        frozen: c.is_frozen()?,
        archived: c.is_archived()?,
        persona_changed: matches!(c.persona_status(), PersonaStatus::Changed { .. }),
        session,
        nights_waiting: c.unconsolidated()?.len(),
        mood: c.mood()?,
        note: c.last_turn_note()?.map(|(_, note)| note),
        sections,
    })
}

async fn overview(State(console): Shared, Route(id): Route<Uuid>) -> Answer<Overview> {
    blocking(move || console.read(id, overview_of)).await
}

async fn memories(State(console): Shared, Route(id): Route<Uuid>) -> Answer<Vec<Memory>> {
    blocking(move || console.read(id, Companion::memories)).await
}

async fn memory(State(console): Shared, Route((id, memory)): Route<(Uuid, i64)>) -> Answer<Detail> {
    blocking(move || {
        console.read(id, |c| {
            // Says so when there is no such memory, instead of two empty lists.
            c.memory(memory)?;
            Ok(Detail {
                versions: c.memory_versions(memory)?,
                turns: c.source_turns(memory)?,
            })
        })
    })
    .await
}

async fn facts(State(console): Shared, Route(id): Route<Uuid>) -> Answer<Vec<FactIssue>> {
    blocking(move || console.read(id, Companion::check_facts)).await
}

async fn recall(
    State(console): Shared,
    Route(id): Route<Uuid>,
    Query(query): Query<RecallQuery>,
) -> Answer<Found> {
    blocking(move || {
        console.read(id, |c| {
            let ranking = c.ranking();
            Ok(Found {
                // `search`, like `clanka recall`: nothing is marked as recalled.
                found: c.search(&query.q, ranking)?,
                limit: ranking.limit,
                min_relevance: ranking.min_relevance,
                min_score: ranking.min_score,
            })
        })
    })
    .await
}

async fn log(
    State(console): Shared,
    Route(id): Route<Uuid>,
    Query(query): Query<LogQuery>,
) -> Answer<Vec<WriteEntry>> {
    blocking(move || {
        // `writes_since` is exclusive, as `clanka doctor log` knows.
        console.read(id, |c| {
            c.writes_since(query.since.map_or(i64::MIN, |at| at - 1))
        })
    })
    .await
}

/// A browser on this machine reaches the server as 127.0.0.1 or localhost,
/// whatever port a tunnel gave it. Any other name is another site's page
/// pointing its own domain here to read the answers.
fn local(host: &str) -> bool {
    let name = host.rsplit_once(':').map_or(host, |(name, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) {
            name
        } else {
            host
        }
    });
    name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost")
}

/// Whether two texts are the same, in time that depends only on their lengths.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |different, (x, y)| different | (x ^ y))
            == 0
}

/// Why a request is turned away, or `None` to let it through. The page's
/// files are no secret; the API is.
fn refusal(console: &Console, request: &Request) -> Option<Failure> {
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|value: &HeaderValue| value.to_str().ok())
    };
    if !header(header::HOST).is_some_and(local) {
        return Some(Failure::Forbidden);
    }
    if request.uri().path().starts_with("/api/") {
        let token = header(header::AUTHORIZATION).and_then(|value| value.strip_prefix("Bearer "));
        if !token.is_some_and(|token| same(token, &console.token)) {
            return Some(Failure::Unauthorized);
        }
    }
    None
}

async fn guard(State(console): Shared, request: Request, next: Next) -> Response {
    match refusal(&console, &request) {
        Some(failure) => failure.into_response(),
        None => next.run(request).await,
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Everything that is not the API: the page's files. A path that names no
/// file is a place inside the page, which the page's own routing reads.
async fn asset(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    if path.starts_with("/api/") {
        return Failure::Missing(format!("no {path} in this clanka")).into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let file = |path: &str| ASSETS.iter().find(|(route, ..)| *route == path);
    let is_place = !path
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'));
    let found = file(path).or_else(|| is_place.then(|| file("/index.html")).flatten());
    let Some(&(route, tag, bytes)) = found else {
        let why = if ASSETS.is_empty() {
            "This clanka was built without its page: web/dist was missing."
        } else {
            "No such file."
        };
        return (StatusCode::NOT_FOUND, why).into_response();
    };
    let fresh = [
        (header::ETAG, tag),
        // The files keep their names across builds, so the browser asks each time.
        (header::CACHE_CONTROL, "no-cache"),
        (header::CONTENT_TYPE, content_type(route)),
    ];
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|seen| seen.as_bytes() == tag.as_bytes())
    {
        return (StatusCode::NOT_MODIFIED, fresh).into_response();
    }
    (fresh, bytes).into_response()
}

pub fn router(console: Arc<Console>) -> Router {
    Router::new()
        .route("/api/companions", get(companions))
        .route("/api/companions/{id}", get(overview))
        .route("/api/companions/{id}/memories", get(memories))
        .route("/api/companions/{id}/memories/{memory}", get(memory))
        .route("/api/companions/{id}/facts", get(facts))
        .route("/api/companions/{id}/recall", get(recall))
        .route("/api/companions/{id}/log", get(log))
        .fallback(asset)
        .layer(middleware::from_fn_with_state(console.clone(), guard))
        .with_state(console)
}

/// The address to open: the token rides in the fragment, which a browser
/// keeps to itself, so it is in no request line and no `Referer`.
fn address(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/#t={token}")
}

fn open_in_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // No browser to open (a machine reached over SSH) is not a failure: the
    // address is printed either way.
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Serves the page until the process is stopped.
pub fn serve(paths: Paths, port: u16, browser: bool) -> Result<()> {
    let token = setup::random_hex(32);
    // The embedding model loads with the first companion opened, not before
    // the page is up, and once for all of them.
    let embedder: Mutex<Option<SharedEmbedder>> = Mutex::new(None);
    let opener = Paths::get()?;
    let console = Arc::new(Console::new(paths, token.clone(), move |soul| {
        let embedder = {
            let mut loaded = lock(&embedder);
            match &*loaded {
                Some(embedder) => embedder.clone(),
                None => loaded.insert(SharedEmbedder::load(&opener)?).clone(),
            }
        };
        setup::open(&opener, soul, embedder, None)
    }));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .with_context(|| {
                format!("listening on 127.0.0.1:{port}; pick another port with --port")
            })?;
        let url = address(listener.local_addr()?.port(), &token);
        println!("clanka web is at {url}");
        println!("It only reads. Whoever has this address can read every memory here.");
        println!("Stop it with Ctrl-C.");
        if browser {
            open_in_browser(&url);
        }
        axum::serve(listener, router(console)).await?;
        Ok(())
    })
}
