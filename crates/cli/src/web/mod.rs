//! `clanka web`: a page on this machine for looking through what companions
//! remember. Unless it was started with `--doctor` the server only reads:
//! every route is a GET, and nothing they run calls a write of the core. It
//! listens on 127.0.0.1 alone, and the API answers only requests that carry
//! the token made for this run, because what it serves is what was said to a
//! companion.
//!
//! `--doctor` adds the repairs of `clanka doctor` as POST routes, each the
//! core call its command makes. `delete-all` and `reembed` are not among
//! them. A POST is answered only to the page this server sent.
//!
//! `--tailnet` lets the page be reached through Tailscale Serve as well,
//! under the names Serve forwards to this port, still with the run's token.
//!
//! The page itself is `web/` in the repository, built ahead of time and put
//! into the binary by `build.rs`.

use anyhow::{Context, Result};
use axum::extract::{Path as Route, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clanka::setup::{self, Config, Paths, SharedEmbedder};
use clanka_core::diagnostics::{Event, ms_since};
use clanka_core::doctor::{FactIssue, REINTERPRET_BATCH, WriteEntry};
use clanka_core::model::{Section, SessionEnd};
use clanka_core::soul::Soul;
use clanka_core::{
    Companion, Deletion, Memory, MemoryVersion, Mood, PersonaStatus, Recalled, Rollback,
    SectionVersion, SourceTurn,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
type Handles = Mutex<HashMap<Uuid, Arc<Mutex<Companion>>>>;

/// What the server reads from: the companions set up here, each opened the
/// first time the page asks for it and kept open after.
pub struct Console {
    paths: Paths,
    open: Open,
    token: String,
    opened: Handles,
    /// What `--doctor` adds; `None` on a run that only reads.
    doctor: Option<Doctor>,
    /// The names Tailscale Serve reaches this server at, without a port;
    /// empty unless the run was started with `--tailnet`.
    tailnet: Vec<String>,
}

/// The part of a run that repairs. Its routes use the handles the reads use,
/// which have no model; re-reading memories opens a second one on the
/// `[model]`, so a read never waits behind a call to it.
pub struct Doctor {
    /// The `[model]` in config.toml, by name, and a companion opened on it.
    model: Option<(String, Open)>,
    rereaders: Handles,
}

impl Doctor {
    pub fn new(model: Option<(String, Open)>) -> Self {
        Self {
            model,
            rereaders: Mutex::new(HashMap::new()),
        }
    }
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
            doctor: None,
            tailnet: Vec::new(),
        }
    }

    /// The same console, able to repair.
    pub fn with_doctor(mut self, doctor: Doctor) -> Self {
        self.doctor = Some(doctor);
        self
    }

    /// The same console, answering also at these names, which Tailscale Serve
    /// sends on over HTTPS.
    pub fn with_tailnet(mut self, names: Vec<String>) -> Self {
        self.tailnet = names;
        self
    }

    /// The scheme a browser reaches the server with at `host`: `http` on this
    /// machine, `https` through Tailscale Serve. `None` for any other name,
    /// which is another site's page pointing its own domain here to read the
    /// answers. The port does not matter: a tunnel or Serve may give another.
    fn scheme(&self, host: &str) -> Option<&'static str> {
        let name = host.rsplit_once(':').map_or(host, |(name, port)| {
            if port.bytes().all(|b| b.is_ascii_digit()) {
                name
            } else {
                host
            }
        });
        if name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost") {
            Some("http")
        } else if self.tailnet.iter().any(|t| name.eq_ignore_ascii_case(t)) {
            Some("https")
        } else {
            None
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
        self.handle(id, &self.opened, &self.open)
    }

    /// A handle from `kept`, or one `open` makes and `kept` keeps.
    fn handle(
        &self,
        id: Uuid,
        kept: &Handles,
        open: &Open,
    ) -> Result<Arc<Mutex<Companion>>, Failure> {
        if let Some(handle) = lock(kept).get(&id) {
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
        let companion = open(&soul).map_err(broken)?;
        Ok(lock(kept)
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
        let companion = caught_up(id, &handle, &self.opened)?;
        Ok(read(&companion)?)
    }

    /// Runs a repair on a companion that has caught up with the disk, and
    /// notes it in the diagnostics as the `clanka doctor` command it stands for.
    fn repair<T>(
        &self,
        id: Uuid,
        command: &'static str,
        work: impl FnOnce(&mut Companion) -> clanka_core::Result<T>,
    ) -> Result<T, Failure> {
        let handle = self.companion(id)?;
        let mut companion = caught_up(id, &handle, &self.opened)?;
        noted(&mut companion, command, work)
    }

    /// Has the companion re-read these memories, on a handle that has the model.
    fn reread(&self, id: Uuid, ids: &[i64]) -> Result<Reread, Failure> {
        let doctor = self
            .doctor
            .as_ref()
            .ok_or_else(|| Failure::Missing("this clanka web only reads".into()))?;
        let Some((_, open)) = &doctor.model else {
            return Err(Failure::Refused(format!(
                "no companion model configured to re-read the memories; add a [model] section \
                 to {} and start `clanka web --doctor` again",
                self.paths.config_file().display()
            )));
        };
        if ids.is_empty() || ids.len() > REINTERPRET_BATCH {
            return Err(Failure::Refused(format!(
                "between 1 and {REINTERPRET_BATCH} memories are re-read at a time"
            )));
        }
        let handle = self.handle(id, &doctor.rereaders, open)?;
        let mut companion = caught_up(id, &handle, &doctor.rereaders)?;
        let reflection = noted(&mut companion, "doctor reinterpret", |c| c.reinterpret(ids))?;
        Ok(Reread {
            rewritten: reflection
                .calls
                .iter()
                .filter(|call| call.ok && call.name == "reinterpret")
                .count(),
            model_calls: reflection.rounds,
            refused: reflection
                .calls
                .iter()
                .filter(|call| !call.ok)
                .map(|call| format!("{}: {}", call.name, call.result))
                .collect(),
        })
    }
}

/// The companion behind a handle, caught up with the disk.
fn caught_up<'a>(
    id: Uuid,
    handle: &'a Mutex<Companion>,
    kept: &Handles,
) -> Result<MutexGuard<'a, Companion>, Failure> {
    let mut companion = lock(handle);
    if let Err(e) = companion.refresh() {
        // Its database was replaced or its soul.md no longer loads: the
        // next request opens it again from what is there.
        lock(kept).remove(&id);
        return Err(e.into());
    }
    Ok(companion)
}

/// Runs `work` and notes how it went as a `command` event: its name, how long
/// it took and the kind of error, as a run of the command itself is noted.
fn noted<T>(
    companion: &mut Companion,
    command: &'static str,
    work: impl FnOnce(&mut Companion) -> clanka_core::Result<T>,
) -> Result<T, Failure> {
    let started = std::time::Instant::now();
    let done = work(companion);
    companion.diagnose(&Event::Command {
        name: command,
        ms: ms_since(started),
        outcome: if done.is_ok() { "ok" } else { "error" },
        error: done.as_ref().err().map(clanka_core::Error::code),
    });
    Ok(done?)
}

/// A refusal, as the status and the `{"error": …}` the page shows.
#[derive(Debug)]
enum Failure {
    Unauthorized,
    Forbidden,
    /// A change asked for by something other than this server's own page.
    Foreign,
    Missing(String),
    /// The core will not do it as things stand: not frozen, seared, archived.
    Refused(String),
    Broken(String),
}

fn broken(error: impl std::fmt::Display) -> Failure {
    Failure::Broken(format!("{error:#}"))
}

impl From<clanka_core::Error> for Failure {
    fn from(error: clanka_core::Error) -> Self {
        match error {
            clanka_core::Error::NotFound(_) => Failure::Missing(error.to_string()),
            clanka_core::Error::NotFrozen
            | clanka_core::Error::Frozen
            | clanka_core::Error::Archived
            | clanka_core::Error::PersonaUnresolved
            | clanka_core::Error::SessionClosed(_)
            | clanka_core::Error::Seared(_)
            | clanka_core::Error::Invalid(_) => Failure::Refused(error.to_string()),
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
                "clanka web answers only as 127.0.0.1, localhost or the tailnet name it printed."
                    .into(),
            ),
            Failure::Foreign => (
                StatusCode::FORBIDDEN,
                "clanka web changes a memory only when its own page asks.".into(),
            ),
            Failure::Missing(error) => (StatusCode::NOT_FOUND, error),
            Failure::Refused(error) => (StatusCode::CONFLICT, error),
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
    /// Whether this run was started with `--doctor`, so the page can repair.
    can_repair: bool,
    /// The `[model]` that re-reads memories on such a run, if there is one.
    model: Option<String>,
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

#[derive(Serialize)]
struct Frozen {
    frozen: bool,
}

#[derive(Serialize)]
struct Ended {
    /// What was open and no longer is: `dream`, `conversation` or `none`.
    ended: &'static str,
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

#[derive(Deserialize)]
struct IdsQuery {
    /// Memory IDs with commas between them.
    ids: String,
}

#[derive(Deserialize)]
struct RollbackQuery {
    /// Milliseconds since the epoch; writes at or after it are undone.
    to: i64,
}

/// What a rollback would do, and the newest entry of the write log it was
/// worked out against. The page sends `seen` back with the rollback itself.
#[derive(Serialize)]
struct RollbackPlan {
    plan: Rollback,
    seen: i64,
}

#[derive(Deserialize)]
struct RollbackOrder {
    to: i64,
    seen: i64,
}

#[derive(Serialize)]
struct Reread {
    rewritten: usize,
    model_calls: usize,
    /// Calls of the model that were refused, as `clanka doctor reinterpret` prints them.
    refused: Vec<String>,
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
        can_repair: false,
        model: None,
    })
}

async fn overview(State(console): Shared, Route(id): Route<Uuid>) -> Answer<Overview> {
    blocking(move || {
        let mut overview = console.read(id, overview_of)?;
        if let Some(doctor) = &console.doctor {
            overview.can_repair = true;
            overview.model = doctor.model.as_ref().map(|(name, _)| name.clone());
        }
        Ok(overview)
    })
    .await
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

async fn freeze(State(console): Shared, Route(id): Route<Uuid>) -> Answer<Frozen> {
    blocking(move || {
        console.repair(id, "doctor freeze", |c| c.set_frozen(true))?;
        Ok(Frozen { frozen: true })
    })
    .await
}

async fn unfreeze(State(console): Shared, Route(id): Route<Uuid>) -> Answer<Frozen> {
    blocking(move || {
        console.repair(id, "doctor unfreeze", |c| c.set_frozen(false))?;
        Ok(Frozen { frozen: false })
    })
    .await
}

/// `clanka doctor end-session`: an open dream is dropped, else the open
/// conversation ends without a night.
async fn end_session(State(console): Shared, Route(id): Route<Uuid>) -> Answer<Ended> {
    blocking(move || {
        let ended = console.repair(id, "doctor end-session", |c| {
            if c.drop_dream()?.is_some() {
                return Ok("dream");
            }
            match c.session_in_progress()? {
                Some(session) => {
                    c.end_session(session, SessionEnd::Doctor)?;
                    Ok("conversation")
                }
                None => Ok("none"),
            }
        })?;
        Ok(Ended { ended })
    })
    .await
}

async fn delete_plan(
    State(console): Shared,
    Route(id): Route<Uuid>,
    Query(query): Query<IdsQuery>,
) -> Answer<Deletion> {
    blocking(move || {
        let ids = query
            .ids
            .split(',')
            .map(|id| id.trim().parse::<i64>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Failure::Refused(format!("{:?} is not a list of memories", query.ids)))?;
        console.read(id, |c| c.delete_plan(&ids))
    })
    .await
}

async fn delete(
    State(console): Shared,
    Route(id): Route<Uuid>,
    Json(order): Json<Ids>,
) -> Answer<Deletion> {
    blocking(move || {
        console.repair(id, "doctor delete", |c| {
            // All of them or none: one that is gone refuses before any goes.
            c.delete_plan(&order.ids)?;
            c.delete_memories(&order.ids)
        })
    })
    .await
}

async fn rollback_plan(
    State(console): Shared,
    Route(id): Route<Uuid>,
    Query(query): Query<RollbackQuery>,
) -> Answer<RollbackPlan> {
    blocking(move || {
        console.read(id, |c| {
            Ok(RollbackPlan {
                // `rollback_to` is exclusive, as `clanka doctor rollback` knows.
                plan: c.rollback_plan(query.to - 1)?,
                seen: c.write_log_head()?,
            })
        })
    })
    .await
}

async fn rollback(
    State(console): Shared,
    Route(id): Route<Uuid>,
    Json(order): Json<RollbackOrder>,
) -> Answer<Rollback> {
    blocking(move || {
        console
            .repair(id, "doctor rollback", |c| {
                if c.write_log_head()? != order.seen {
                    return Ok(None);
                }
                c.rollback_to(order.to - 1).map(Some)
            })?
            .ok_or_else(|| {
                Failure::Refused(
                    "The memory was written to after this rollback was worked out. Look at \
                     what it would do now."
                        .into(),
                )
            })
    })
    .await
}

async fn reinterpret(
    State(console): Shared,
    Route(id): Route<Uuid>,
    Json(order): Json<Ids>,
) -> Answer<Reread> {
    blocking(move || console.reread(id, &order.ids)).await
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
    let Some((host, scheme)) =
        header(header::HOST).and_then(|host| Some((host, console.scheme(host)?)))
    else {
        return Some(Failure::Forbidden);
    };
    if request.uri().path().starts_with("/api/") {
        let token = header(header::AUTHORIZATION).and_then(|value| value.strip_prefix("Bearer "));
        if !token.is_some_and(|token| same(token, &console.token)) {
            return Some(Failure::Unauthorized);
        }
    }
    // A browser names the page a POST comes from. On a run that repairs, only
    // this server's own page may ask for a change, token or no token.
    let reads = matches!(*request.method(), Method::GET | Method::HEAD);
    if console.doctor.is_some()
        && !reads
        && header(header::ORIGIN) != Some(format!("{scheme}://{host}").as_str())
    {
        return Some(Failure::Foreign);
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

/// What the page may load and where it may be shown: its own files and this
/// server, in no frame. The tab holds a token that reads every memory, and on
/// a `--doctor` run deletes them, so nothing from elsewhere runs beside it.
const POLICY: &str = "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; \
    base-uri 'none'; form-action 'none'";

/// Everything that is not the API: the page's files. A path that names no
/// file is a place inside the page, which the page's own routing reads.
async fn asset(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if path.starts_with("/api/") {
        return Failure::Missing(format!("no {path} in this clanka")).into_response();
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
        (header::CONTENT_SECURITY_POLICY, POLICY),
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
    let reads = Router::new()
        .route("/api/companions", get(companions))
        .route("/api/companions/{id}", get(overview))
        .route("/api/companions/{id}/memories", get(memories))
        .route("/api/companions/{id}/memories/{memory}", get(memory))
        .route("/api/companions/{id}/facts", get(facts))
        .route("/api/companions/{id}/recall", get(recall))
        .route("/api/companions/{id}/log", get(log));
    // A GET of a repair says what it would do; a POST does it.
    let repairs = Router::new()
        .route("/api/companions/{id}/freeze", post(freeze))
        .route("/api/companions/{id}/unfreeze", post(unfreeze))
        .route("/api/companions/{id}/end-session", post(end_session))
        .route("/api/companions/{id}/delete", get(delete_plan).post(delete))
        .route(
            "/api/companions/{id}/rollback",
            get(rollback_plan).post(rollback),
        )
        .route("/api/companions/{id}/reinterpret", post(reinterpret));
    let routes = if console.doctor.is_some() {
        reads.merge(repairs)
    } else {
        reads
    };
    routes
        .fallback(asset)
        .layer(middleware::from_fn_with_state(console.clone(), guard))
        .with_state(console)
}

/// The address to open: the token rides in the fragment, which a browser
/// keeps to itself, so it is in no request line and no `Referer`.
fn address(base: &str, token: &str) -> String {
    format!("{base}/#t={token}")
}

/// What `tailscale serve status --json` says Serve does here.
fn serve_status() -> Result<Value> {
    let mut clis = vec![PathBuf::from("tailscale")];
    if cfg!(target_os = "macos") {
        // The app does not always put its command on PATH.
        clis.push("/Applications/Tailscale.app/Contents/MacOS/Tailscale".into());
    }
    for cli in clis {
        let output = match std::process::Command::new(&cli)
            .args(["serve", "status", "--json"])
            .stdin(std::process::Stdio::null())
            .output()
        {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            output => output.with_context(|| format!("running {}", cli.display()))?,
        };
        if !output.status.success() {
            anyhow::bail!(
                "`tailscale serve status --json` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if output.stdout.trim_ascii().is_empty() {
            return Ok(Value::Null);
        }
        return serde_json::from_slice(&output.stdout)
            .context("reading what `tailscale serve status --json` printed");
    }
    anyhow::bail!("--tailnet needs Tailscale, and there is no tailscale command on PATH")
}

/// Where Tailscale Serve sends a browser on to `port` here, from what
/// `tailscale serve status --json` printed: each as the name the browser asks
/// for and the address it opens. Refused when Serve sends it on over plain
/// HTTP, or when Funnel puts it on the internet.
fn forwards(status: &Value, port: u16) -> Result<Vec<(String, String)>> {
    let targets = [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ];
    // A `--bg` setting is at the top; one run in the foreground is under its session.
    let foreground = status.get("Foreground").and_then(Value::as_object);
    let configs = std::iter::once(status).chain(foreground.into_iter().flat_map(|f| f.values()));
    let mut found = Vec::new();
    for config in configs {
        let Some(web) = config.get("Web").and_then(Value::as_object) else {
            continue;
        };
        for (key, entry) in web {
            let proxy = entry
                .get("Handlers")
                .and_then(|handlers| handlers.get("/"))
                .and_then(|handler| handler.get("Proxy"))
                .and_then(Value::as_str);
            if !proxy.is_some_and(|proxy| targets.iter().any(|t| proxy.trim_end_matches('/') == t))
            {
                continue;
            }
            let Some((name, served)) = key.rsplit_once(':') else {
                continue;
            };
            let https = config
                .get("TCP")
                .and_then(|tcp| tcp.get(served))
                .and_then(|listener| listener.get("HTTPS"))
                .and_then(Value::as_bool);
            if https != Some(true) {
                anyhow::bail!(
                    "Tailscale Serve sends {key} on to port {port} without HTTPS. Serve it with \
                     `tailscale serve --bg --https={served} http://127.0.0.1:{port}`."
                );
            }
            let funnel = config
                .get("AllowFunnel")
                .and_then(|funnel| funnel.get(key))
                .and_then(Value::as_bool);
            if funnel == Some(true) {
                anyhow::bail!(
                    "Tailscale Funnel puts {key} on the internet, and clanka web stays on the \
                     tailnet. Turn it off with `tailscale funnel --https={served} off`."
                );
            }
            let base = if served == "443" {
                format!("https://{name}")
            } else {
                format!("https://{key}")
            };
            found.push((name.to_owned(), base));
        }
    }
    if found.is_empty() {
        anyhow::bail!(
            "Tailscale Serve sends nothing on to 127.0.0.1:{port}. Have it do so with \
             `tailscale serve --bg --https={port} http://127.0.0.1:{port}`, then run this again."
        );
    }
    Ok(found)
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

/// Serves the page until the process is stopped. With `doctor` the page can
/// also repair; with `tailnet` it answers through Tailscale Serve too.
pub fn serve(paths: Paths, port: u16, browser: bool, doctor: bool, tailnet: bool) -> Result<()> {
    let tailnet = if tailnet {
        forwards(&serve_status()?, port)?
    } else {
        Vec::new()
    };
    let token = setup::random_hex(32);
    // The embedding model loads with the first companion opened, not before
    // the page is up, and once for all of them.
    let embedder: Arc<Mutex<Option<SharedEmbedder>>> = Arc::default();
    let opener = |model: Option<setup::ModelConfig>| -> Result<Open> {
        let paths = Paths::get()?;
        let embedder = embedder.clone();
        Ok(Box::new(move |soul: &Path| {
            let embedder = {
                let mut loaded = lock(&embedder);
                match &*loaded {
                    Some(embedder) => embedder.clone(),
                    None => loaded.insert(SharedEmbedder::load(&paths)?).clone(),
                }
            };
            setup::open(&paths, soul, embedder, model.as_ref())
        }))
    };
    let model = if doctor {
        Config::load(&paths)?.model
    } else {
        None
    };
    let rereader = match model {
        Some(model) => Some((model.model.clone(), opener(Some(model))?)),
        None => None,
    };
    let mut console = Console::new(paths, token.clone(), opener(None)?)
        .with_tailnet(tailnet.iter().map(|(name, _)| name.clone()).collect());
    if doctor {
        console = console.with_doctor(Doctor::new(rereader));
    }
    let console = Arc::new(console);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .with_context(|| {
                format!("listening on 127.0.0.1:{port}; pick another port with --port")
            })?;
        let port = listener.local_addr()?.port();
        let url = address(&format!("http://127.0.0.1:{port}"), &token);
        println!("clanka web is at {url}");
        for (_, base) in &tailnet {
            println!("On your tailnet: {}", address(base, &token));
        }
        if doctor {
            println!(
                "It can repair: freeze, end a conversation, roll back, and delete or re-read \
                 memories. Whoever has this address can read every memory here and do the same."
            );
        } else {
            println!("It only reads. Whoever has this address can read every memory here.");
            println!("To repair from the page too, run `clanka web --doctor`.");
        }
        if !tailnet.is_empty() {
            println!(
                "Any device on your tailnet that Tailscale lets reach this machine can use the \
                 tailnet address."
            );
        }
        println!("Stop it with Ctrl-C.");
        if browser {
            open_in_browser(&url);
        }
        axum::serve(listener, router(console)).await?;
        Ok(())
    })
}
