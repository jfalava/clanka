//! Program status over OSC 7501: what clanka is doing, for a terminal that
//! shows it (Rex puts it on the tab and in the session picker). One session per
//! run reports to stderr; a terminal that doesn't know the sequence ignores it.
//! The same session draws the live row a person sees ([`crate::ui`]) while a
//! step is `working`.
//!
//! Nothing is written until a command names its first step, so the commands
//! that answer at once stay silent. The feature query is never sent: `clanka
//! chat` reads stdin by the line, and a late answer would arrive as a message.

use crate::ui::Row;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use indicatif::ProgressDrawTarget;
use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

const APP: &str = "clanka";
/// Most bytes of a decoded `title` and `msg` a terminal accepts; a longer one
/// loses the whole report.
const TITLE_LIMIT: usize = 192;
const MSG_LIMIT: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// At rest, waiting for the user.
    Idle,
    Working,
    /// A result is ready that the user may not have seen.
    Done,
    Error,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Error => "error",
        }
    }
}

/// How a run ended.
pub enum Outcome {
    /// With the [`Session::summary`] as its message, if one was given.
    Done,
    /// Interrupted or left: nothing for the user to look at.
    Idle,
    /// With what went wrong.
    Error(String),
}

/// How far a step is: `done` of `total`, things or bytes. A `total` of 0 means
/// there is no telling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Amount {
    pub done: usize,
    pub total: usize,
    pub bytes: bool,
}

impl Amount {
    /// Floored, so 100 is said only of the whole.
    pub(crate) fn percent(self) -> Option<u8> {
        (self.total > 0).then(|| (self.done.min(self.total) * 100 / self.total) as u8)
    }
}

/// One run's reports. Clones share it, so threads report to the same records.
#[derive(Clone)]
pub struct Session(Arc<Mutex<Inner>>);

struct Inner {
    /// Where reports go; `None` when they are off or a write has failed.
    sink: Option<Box<dyn Write + Send>>,
    /// The live row, when a person is there to see one.
    row: Option<Row>,
    /// The step has printed, or is about to: its row stays away.
    quiet: bool,
    begun: bool,
    closed: bool,
    state: State,
    label: String,
    amount: Option<Amount>,
    detail: Option<String>,
    /// Shown instead of the detail while a model request waits to be retried.
    retry: Option<String>,
    /// Model calls made in this step, when the step counts them.
    calls: Option<usize>,
    summary: Option<String>,
    last: String,
}

impl Session {
    /// A session that reports nothing.
    pub fn off() -> Self {
        Self::new(None, None)
    }

    /// A session that reports to `sink`.
    pub fn to(sink: impl Write + Send + 'static) -> Self {
        Self::new(Some(Box::new(sink)), None)
    }

    /// A session on stderr when a terminal is there: the live row, and the
    /// reports unless `CLANKA_PROGRAM_STATUS` turns them off.
    fn terminal() -> Self {
        let stderr = std::io::stderr();
        let var = |name| std::env::var(name).ok();
        let (terminal, term) = (stderr.is_terminal(), var("TERM"));
        let reports = wanted(
            terminal,
            term.as_deref(),
            var("CLANKA_PROGRAM_STATUS").as_deref(),
        );
        Self::new(
            reports.then(|| Box::new(stderr) as Box<dyn Write + Send>),
            wanted(terminal, term.as_deref(), None).then(|| {
                Row::new(
                    ProgressDrawTarget::stderr,
                    crate::ui::look().err,
                    crate::ui::glyphs,
                )
            }),
        )
    }

    fn new(sink: Option<Box<dyn Write + Send>>, row: Option<Row>) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            sink,
            row,
            quiet: false,
            begun: false,
            closed: false,
            state: State::Idle,
            label: String::new(),
            amount: None,
            detail: None,
            retry: None,
            calls: None,
            summary: None,
            last: String::new(),
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether reports go anywhere.
    pub fn enabled(&self) -> bool {
        self.lock().sink.is_some()
    }

    /// Whether a terminal is there, for reports or for the live row.
    pub fn on_terminal(&self) -> bool {
        let s = self.lock();
        s.sink.is_some() || s.row.is_some()
    }

    /// Starts a step: `working` under `label`, with no percentage or detail yet.
    pub fn phase(&self, label: &str) {
        self.report(State::Working, label, None);
    }

    /// The root record's state, its label and a line of detail. The first
    /// call is what turns reporting on for this run.
    pub fn report(&self, state: State, label: &str, msg: Option<&str>) {
        let mut s = self.lock();
        if !s.closed {
            s.begun = true;
            s.set(state, label, msg);
        }
    }

    /// How far the step is. A `total` of 0 means there is no telling.
    pub fn progress(&self, done: usize, total: usize) {
        let bytes = false;
        self.amount(Amount { done, total, bytes });
    }

    /// [`Session::progress`] for a download: the row shows megabytes.
    pub fn bytes(&self, done: usize, total: usize) {
        let bytes = true;
        self.amount(Amount { done, total, bytes });
    }

    fn amount(&self, amount: Amount) {
        let mut s = self.lock();
        if s.live() && s.state == State::Working && s.amount != Some(amount) {
            s.amount = Some(amount);
            s.root();
        }
    }

    /// Takes the live row off the screen until the next step is named: the
    /// step is over, or what it writes from here on is its own to show.
    pub fn quiet(&self) {
        let mut s = self.lock();
        s.quiet = true;
        s.draw();
    }

    /// Runs `write` with the live row out of the way, so a line it prints
    /// lands above the row instead of in it. For a line or two: the row
    /// stands still meanwhile, and `write` must not report to this session.
    /// Something slower, or a child that has the terminal, goes after
    /// [`Session::quiet`].
    pub fn above<T>(&self, write: impl FnOnce() -> T) -> T {
        // The bar is taken out first: the session isn't locked during `write`.
        let bar = self.lock().row.as_ref().and_then(Row::bar);
        match bar {
            Some(bar) => bar.suspend(write),
            None => write(),
        }
    }

    /// A line under the step's label; an empty one clears it.
    pub fn detail(&self, msg: &str) {
        let mut s = self.lock();
        if s.live() {
            s.detail = Some(text(msg, MSG_LIMIT)).filter(|m| !m.is_empty());
            s.root();
        }
    }

    /// Makes this step's detail the number of model calls it has made.
    pub fn count_model_calls(&self) {
        self.lock().calls = Some(0);
    }

    /// A model request is going out.
    pub fn model_call(&self) {
        let mut s = self.lock();
        if s.live()
            && let Some(calls) = s.calls.map(|calls| calls + 1)
        {
            s.calls = Some(calls);
            s.detail = Some(format!("model call {calls}"));
            s.root();
        }
    }

    /// A model request failed and waits to be tried again; `None` once it is
    /// through or given up on.
    pub fn retrying(&self, msg: Option<&str>) {
        let mut s = self.lock();
        let retry = msg.map(|m| text(m, MSG_LIMIT)).filter(|m| !m.is_empty());
        if s.live() && s.retry != retry {
            s.retry = retry;
            s.root();
        }
    }

    /// What a run that ends well says it did.
    pub fn summary(&self, msg: &str) {
        self.lock().summary = Some(msg.to_owned());
    }

    /// A record beside the root's, for work that goes on while the root is
    /// busy with something else. `id` is letters, digits and `_.+-`.
    pub fn child(&self, id: &str, state: State, title: &str, msg: &str) {
        let mut s = self.lock();
        if s.live() {
            let record = record(
                state,
                Some(id),
                &text(title, TITLE_LIMIT),
                &text(msg, MSG_LIMIT),
                None,
            );
            s.write(record);
        }
    }

    /// Removes a [`Session::child`] record.
    pub fn clear(&self, id: &str) {
        let mut s = self.lock();
        if s.live() {
            s.write(format!("\x1b]7501;state=clear:id={id}\x1b\\"));
        }
    }

    /// The run's last report. Only the first close counts.
    pub fn close(&self, outcome: Outcome) {
        let mut s = self.lock();
        if !s.live() {
            s.closed = true;
            return;
        }
        s.retry = None;
        let summary = s.summary.take();
        match outcome {
            Outcome::Done => s.set(State::Done, "Done", summary.as_deref()),
            Outcome::Idle => s.set(State::Idle, "Stopped", None),
            Outcome::Error(why) => s.set(State::Error, "Failed", Some(&why)),
        }
        s.closed = true;
    }
}

impl Inner {
    fn live(&self) -> bool {
        self.begun && !self.closed
    }

    fn set(&mut self, state: State, label: &str, msg: Option<&str>) {
        let label = text(label, TITLE_LIMIT);
        if state != self.state || label != self.label {
            self.amount = None;
            self.calls = None;
        }
        self.quiet = false;
        self.state = state;
        self.label = label;
        self.detail = msg.map(|m| text(m, MSG_LIMIT)).filter(|m| !m.is_empty());
        self.root();
    }

    /// Reports the root record. A report replaces its record, so each carries
    /// everything. The window title says the same: Rex labels the root from it.
    fn root(&mut self) {
        self.draw();
        let percent = self
            .amount
            .filter(|_| self.state == State::Working)
            .and_then(Amount::percent);
        let suffix = percent.map(|p| format!(" {p}%")).unwrap_or_default();
        let title = text(
            &format!("{APP}: {}", self.label),
            TITLE_LIMIT - suffix.len(),
        ) + &suffix;
        let msg = self
            .retry
            .as_ref()
            .or(self.detail.as_ref())
            .unwrap_or(&self.label);
        let record = record(self.state, None, &title, msg, percent);
        self.write(format!("\x1b]2;{title}\x1b\\{record}"));
    }

    /// The live row: up while a step is working, unless the step went quiet.
    fn draw(&mut self) {
        let up = self.live() && self.state == State::Working && !self.quiet;
        let Some(row) = &mut self.row else {
            return;
        };
        if up {
            let detail = self.retry.as_deref().or(self.detail.as_deref());
            row.show(&self.label, self.amount, detail);
        } else {
            row.clear();
        }
    }

    /// Writes a report unless it repeats the last one. A write that fails
    /// ends reporting: status never fails the command.
    fn write(&mut self, report: String) {
        if report == self.last {
            return;
        }
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        if sink
            .write_all(report.as_bytes())
            .and_then(|()| sink.flush())
            .is_ok()
        {
            self.last = report;
        } else {
            self.sink = None;
        }
    }
}

fn record(state: State, id: Option<&str>, title: &str, msg: &str, percent: Option<u8>) -> String {
    let mut pairs = format!("state={}", state.as_str());
    if let Some(id) = id {
        pairs.push_str(&format!(":id={id}"));
    }
    pairs.push_str(&format!(":app={APP}:title={}", STANDARD.encode(title)));
    if !msg.is_empty() {
        pairs.push_str(&format!(":msg={}", STANDARD.encode(msg)));
    }
    if let Some(percent) = percent {
        pairs.push_str(&format!(":progress={percent}"));
    }
    format!("\x1b]7501;{pairs}\x1b\\")
}

/// `raw` as one line a terminal accepts: control characters and the bidi
/// overrides become spaces (a model's or a provider's text must not carry
/// terminal commands), runs of space collapse, and it stops at the last whole
/// character within `limit` bytes.
fn text(raw: &str, limit: usize) -> String {
    let unsafe_char =
        |c: char| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let clean: String = raw
        .chars()
        .map(|c| if unsafe_char(c) { ' ' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = 0;
    for (at, c) in clean.char_indices() {
        if at + c.len_utf8() > limit {
            break;
        }
        end = at + c.len_utf8();
    }
    clean[..end].trim_end().to_owned()
}

/// Whether to report: stderr is a terminal that reads escape sequences, and
/// `CLANKA_PROGRAM_STATUS` isn't `0`.
fn wanted(terminal: bool, term: Option<&str>, setting: Option<&str>) -> bool {
    terminal && term != Some("dumb") && setting != Some("0")
}

static RUN: OnceLock<Session> = OnceLock::new();

/// Starts this process's session, on stderr. Until the binary calls this,
/// [`session`] and the functions below report nothing, so tests stay quiet.
pub fn start() -> Session {
    RUN.get_or_init(Session::terminal).clone()
}

/// This process's session.
pub fn session() -> Session {
    RUN.get().cloned().unwrap_or_else(Session::off)
}

/// [`Session::phase`] on this process's session.
pub fn phase(label: &str) {
    session().phase(label);
}

/// [`Session::progress`] on this process's session.
pub fn progress(done: usize, total: usize) {
    session().progress(done, total);
}

/// [`Session::bytes`] on this process's session.
pub fn bytes(done: usize, total: usize) {
    session().bytes(done, total);
}

/// [`Session::quiet`] on this process's session.
pub fn quiet() {
    session().quiet();
}

/// [`Session::detail`] on this process's session.
pub fn detail(msg: &str) {
    session().detail(msg);
}

/// [`Session::summary`] on this process's session.
pub fn summary(msg: &str) {
    session().summary(msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl Buffer {
        fn take(&self) -> String {
            String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
        }
    }
    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn session() -> (Session, Buffer) {
        let buffer = Buffer::default();
        (Session::to(buffer.clone()), buffer)
    }

    /// The base64 here was made with `printf '%s' … | base64`, not by this code.
    #[test]
    fn a_run_reports_each_step_and_how_it_ended() {
        let (s, written) = session();
        s.phase("Re-reading memories");
        assert_eq!(
            written.take(),
            "\x1b]2;clanka: Re-reading memories\x1b\\\x1b]7501;state=working:app=clanka:\
             title=Y2xhbmthOiBSZS1yZWFkaW5nIG1lbW9yaWVz:msg=UmUtcmVhZGluZyBtZW1vcmllcw==\x1b\\"
        );
        // 40 of 120 is 33.3%: the percentage is floored, in the title too.
        s.progress(40, 120);
        s.count_model_calls();
        s.model_call();
        s.model_call();
        s.model_call();
        let reports = written.take();
        assert!(
            reports.ends_with(
                "\x1b]2;clanka: Re-reading memories 33%\x1b\\\x1b]7501;state=working:app=clanka:\
                 title=Y2xhbmthOiBSZS1yZWFkaW5nIG1lbW9yaWVzIDMzJQ==:msg=bW9kZWwgY2FsbCAz:\
                 progress=33\x1b\\"
            ),
            "{reports:?}"
        );
        // A request waiting on a retry shows over the detail, which comes back.
        s.retrying(Some("HTTP 529; trying again in 2.0s"));
        assert!(
            written
                .take()
                .contains(":msg=SFRUUCA1Mjk7IHRyeWluZyBhZ2FpbiBpbiAyLjBz:")
        );
        s.retrying(None);
        assert!(written.take().contains(":msg=bW9kZWwgY2FsbCAz:"));
        // More than the whole is the whole, and a repeat is not sent twice.
        s.progress(500, 120);
        assert!(written.take().ends_with(":progress=100\x1b\\"));
        s.progress(120, 120);
        s.detail("model call 3");
        assert_eq!(written.take(), "");
        // A finished run carries no percentage, and nothing follows it.
        s.summary("12 memories re-read");
        s.close(Outcome::Done);
        assert_eq!(
            written.take(),
            "\x1b]2;clanka: Done\x1b\\\x1b]7501;state=done:app=clanka:title=Y2xhbmthOiBEb25l:\
             msg=MTIgbWVtb3JpZXMgcmUtcmVhZA==\x1b\\"
        );
        s.phase("More");
        s.close(Outcome::Error("late".into()));
        assert_eq!(written.take(), "");
    }

    #[test]
    fn nothing_is_written_before_the_first_step() {
        let (s, written) = session();
        s.progress(1, 2);
        s.detail("round 1");
        s.count_model_calls();
        s.model_call();
        s.retrying(Some("HTTP 503"));
        s.child("reflect", State::Working, "Reflecting", "turns 1–2");
        s.clear("reflect");
        s.summary("Created Nova");
        s.close(Outcome::Error("no such file".into()));
        assert_eq!(written.take(), "");

        let (s, written) = session();
        s.phase("Downloading model.onnx");
        s.close(Outcome::Error("HTTP 404\nnot found".into()));
        let reports = written.take();
        assert!(
            reports.ends_with(
                "\x1b]2;clanka: Failed\x1b\\\x1b]7501;state=error:app=clanka:\
                 title=Y2xhbmthOiBGYWlsZWQ=:msg=SFRUUCA0MDQgbm90IGZvdW5k\x1b\\"
            ),
            "{reports:?}"
        );

        let (s, written) = session();
        s.report(State::Idle, "Chatting with Nova", None);
        s.close(Outcome::Idle);
        assert!(written.take().ends_with(
            "\x1b]2;clanka: Stopped\x1b\\\x1b]7501;state=idle:app=clanka:\
                 title=Y2xhbmthOiBTdG9wcGVk:msg=U3RvcHBlZA==\x1b\\"
        ));
    }

    #[test]
    fn a_child_record_is_reported_beside_the_root_and_cleared() {
        let (s, written) = session();
        s.report(State::Idle, "Chatting with Nova", None);
        written.take();
        s.child("reflect", State::Working, "Reflecting", "Turns 3–4");
        assert_eq!(
            written.take(),
            "\x1b]7501;state=working:id=reflect:app=clanka:title=UmVmbGVjdGluZw==:\
             msg=VHVybnMgM+KAkzQ=\x1b\\"
        );
        s.clear("reflect");
        assert_eq!(written.take(), "\x1b]7501;state=clear:id=reflect\x1b\\");
    }

    #[test]
    fn text_from_a_model_cannot_carry_terminal_commands() {
        assert_eq!(
            text(
                "  HTTP 500:\n\x1b]2;owned\x07\tbody \u{202e}reversed\u{2069} \u{9c}end ",
                MSG_LIMIT
            ),
            "HTTP 500: ]2;owned body reversed end"
        );
        // 2 bytes a letter: a cut at 5 bytes keeps two whole letters, not half
        // of the third.
        assert_eq!(text("ééé", 5), "éé");
        assert_eq!(text("a b", 2), "a");

        // A 300-byte label: the title is cut to the limit on a character, with
        // room left for the percentage.
        let (s, written) = session();
        s.phase(&"é".repeat(150));
        s.progress(1, 2);
        let reports = written.take();
        let last = reports.rsplit("title=").next().unwrap();
        let title = STANDARD.decode(last.split(':').next().unwrap()).unwrap();
        assert_eq!(
            String::from_utf8(title).unwrap(),
            format!("clanka: {} 50%", "é".repeat(90))
        );
    }

    #[test]
    fn a_terminal_that_went_away_ends_reporting_without_failing() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let s = Session::to(Closed);
        assert!(s.enabled());
        s.phase("Re-reading memories");
        assert!(!s.enabled());
        s.close(Outcome::Done);
    }

    /// What the live row says: its header and its detail, or `None` when it
    /// is off the screen.
    fn row(s: &Session) -> Option<(String, String)> {
        let bar = s.lock().row.as_ref().and_then(Row::bar)?;
        Some((bar.prefix(), bar.message()))
    }

    #[test]
    fn the_live_row_is_up_while_a_step_works_and_no_longer() {
        // Reports off, as with CLANKA_PROGRAM_STATUS=0: the row needs none.
        let hidden = Row::new(ProgressDrawTarget::hidden, crate::ui::Paint::PLAIN, || {
            crate::ui::Glyphs::Unicode
        });
        let s = Session::new(None, Some(hidden));
        assert!(s.on_terminal() && !s.enabled());
        s.progress(1, 2);
        assert_eq!(row(&s), None);
        s.phase("Re-reading memories");
        assert_eq!(row(&s), Some(("Re-reading memories".into(), String::new())));
        s.progress(40, 120);
        s.count_model_calls();
        s.model_call();
        let header = "Re-reading memories · 40/120 · 33%".to_owned();
        assert_eq!(row(&s), Some((header.clone(), "model call 1".into())));
        // A retry shows over the detail, as in the report.
        s.retrying(Some("HTTP 529; trying again in 2.0s"));
        let retry = "HTTP 529; trying again in 2.0s".to_owned();
        assert_eq!(row(&s), Some((header.clone(), retry)));
        s.retrying(None);
        assert_eq!(row(&s), Some((header, "model call 1".into())));
        // A line printed meanwhile leaves the row as it was.
        assert_eq!(s.above(|| 7), 7);
        assert!(row(&s).is_some());
        // The step went quiet: more of it doesn't bring the row back, the
        // next step does.
        s.quiet();
        assert_eq!(row(&s), None);
        s.progress(80, 120);
        s.detail("model call 2");
        assert_eq!(row(&s), None);
        s.phase("Downloading model.onnx");
        s.bytes(9_240_000, 23_000_000);
        assert_eq!(
            row(&s).unwrap().0,
            "Downloading model.onnx · 9.2/23.0 MB · 40%"
        );
        // Only work has a row: a result, an error and a chat at rest don't.
        for state in [State::Done, State::Error, State::Idle] {
            s.phase("Nova is replying");
            assert!(row(&s).is_some());
            s.report(state, "Nova", None);
            assert_eq!(row(&s), None, "{state:?}");
        }
        s.phase("Nova is falling asleep");
        s.close(Outcome::Done);
        assert_eq!(row(&s), None);
        s.phase("More");
        assert_eq!(row(&s), None);
    }

    #[test]
    fn reports_need_a_terminal_and_can_be_turned_off() {
        assert!(wanted(true, Some("xterm-ghostty"), None));
        assert!(wanted(true, None, Some("1")));
        assert!(!wanted(false, Some("xterm-ghostty"), None));
        assert!(!wanted(true, Some("dumb"), None));
        assert!(!wanted(true, Some("xterm-ghostty"), Some("0")));
    }
}
