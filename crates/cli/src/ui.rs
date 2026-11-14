//! What a person watching the terminal sees while clanka works: one live row
//! on stderr, a spinner beside the step's name with how far it is and how long
//! it has taken, and a line of detail under it. The run's
//! [`Session`](crate::status::Session) draws it from the same calls that
//! report program status, so the two never disagree.
//!
//! The row is there only when stderr is a terminal. Piped, clanka writes what
//! it always wrote.
//!
//! The same holds for the marks and colors on the lines clanka prints: a
//! stream that is a terminal gets them, and one that is piped gets the plain
//! line, byte for byte what it was before there were any.

use crate::status::{self, Amount};
use clap::builder::styling::{AnsiColor, Style};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "];

pub const GREEN: Style = AnsiColor::Green.on_default();
pub const YELLOW: Style = AnsiColor::Yellow.on_default();
pub const RED: Style = AnsiColor::Red.on_default();
pub const CYAN: Style = AnsiColor::Cyan.on_default();
pub const BOLD: Style = Style::new().bold();
pub const DIM: Style = Style::new().dimmed();

/// `[ui] glyphs` in config.toml: which symbols mark the lines and the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GlyphSetting {
    /// Nerd Font symbols where they will draw, plain Unicode elsewhere.
    #[default]
    Auto,
    Nerd,
    Unicode,
}

/// The symbols in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyphs {
    /// From a Nerd Font's private-use range: boxes in a font without them.
    Nerd,
    /// In nearly every monospace font.
    Unicode,
}

/// What one stream gets beyond the plain line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Paint {
    /// Marks before lines: the stream is a terminal that draws them.
    pub marks: bool,
    /// Colors as well: `NO_COLOR` isn't set.
    pub color: bool,
}

impl Paint {
    pub const PLAIN: Self = Self {
        marks: false,
        color: false,
    };

    /// For a stream, from whether it is a terminal, `TERM` and `NO_COLOR`.
    pub fn of(terminal: bool, term: Option<&str>, no_color: Option<&str>) -> Self {
        let marks = terminal && term != Some("dumb");
        Self {
            marks,
            // https://no-color.org: set and not empty, whatever it holds.
            color: marks && no_color.is_none_or(str::is_empty),
        }
    }

    /// `text` in `style`, or as it is where there is no color.
    pub fn paint(self, style: Style, text: &str) -> String {
        if self.color {
            format!("{style}{text}{style:#}")
        } else {
            text.to_owned()
        }
    }
}

/// How this process's output looks: plain until the binary calls [`start`],
/// so tests and the Hermes plugin write plain lines.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub out: Paint,
    pub err: Paint,
    setting: GlyphSetting,
}

static LOOK: OnceLock<Look> = OnceLock::new();
static GLYPHS: OnceLock<Glyphs> = OnceLock::new();

/// Sets how this process's output looks, from the streams and the
/// environment. `setting` is config.toml's `[ui] glyphs`.
pub fn start(setting: GlyphSetting) {
    let var = |name| std::env::var(name).ok();
    let (term, no_color) = (var("TERM"), var("NO_COLOR"));
    let paint = |terminal| Paint::of(terminal, term.as_deref(), no_color.as_deref());
    let _ = LOOK.set(Look {
        out: paint(std::io::stdout().is_terminal()),
        err: paint(std::io::stderr().is_terminal()),
        setting,
    });
}

/// This process's look.
pub fn look() -> Look {
    LOOK.get().copied().unwrap_or(Look {
        out: Paint::PLAIN,
        err: Paint::PLAIN,
        setting: GlyphSetting::Auto,
    })
}

/// The symbols this process uses. Worked out once, and only when something
/// is about to be marked: it may look through the font directories.
pub fn glyphs() -> Glyphs {
    *GLYPHS.get_or_init(|| {
        let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_owned());
        let mut fonts = vec![
            PathBuf::from("/Library/Fonts"),
            PathBuf::from("/usr/share/fonts"),
        ];
        if let Some(home) = home {
            fonts.push(home.join("Library/Fonts"));
            fonts.push(home.join(".local/share/fonts"));
        }
        detect(look().setting, |name| std::env::var(name).ok(), &fonts)
    })
}

/// Which symbols `setting` means here. No escape sequence asks a terminal for
/// its font, so `auto` goes by what can be told: a terminal that draws Nerd
/// Font symbols itself whatever font it is set to (Ghostty, kitty, WezTerm,
/// and Rex, which is assumed to as it is built on Ghostty's renderer), or a
/// Nerd Font installed on this machine. Over SSH the font is on the other
/// machine, so only the terminal counts there.
pub fn detect(
    setting: GlyphSetting,
    env: impl Fn(&str) -> Option<String>,
    font_dirs: &[PathBuf],
) -> Glyphs {
    match setting {
        GlyphSetting::Nerd => return Glyphs::Nerd,
        GlyphSetting::Unicode => return Glyphs::Unicode,
        GlyphSetting::Auto => {}
    }
    let set = |name| env(name).is_some_and(|value| !value.is_empty());
    let term = env("TERM").unwrap_or_default();
    let program = env("TERM_PROGRAM").unwrap_or_default();
    let draws_them = matches!(term.as_str(), "xterm-ghostty" | "xterm-kitty" | "xterm-rex")
        || matches!(program.as_str(), "ghostty" | "WezTerm")
        || set("KITTY_WINDOW_ID")
        || set("REX_SESSION");
    if draws_them || (!set("SSH_CONNECTION") && font_dirs.iter().any(|dir| has_nerd_font(dir, 3))) {
        Glyphs::Nerd
    } else {
        Glyphs::Unicode
    }
}

/// Whether a file named as a Nerd Font is in `dir`, at most `depth` deep.
/// The patched fonts and the symbols-only one all have `Nerd` in their names.
fn has_nerd_font(dir: &Path, depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut below = Vec::new();
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().contains("Nerd") {
            return true;
        }
        if depth > 0 && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            below.push(entry.path());
        }
    }
    below.iter().any(|dir| has_nerd_font(dir, depth - 1))
}

/// What a marked line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Success,
    Warning,
    Error,
}

impl Mark {
    fn glyph(self, glyphs: Glyphs) -> &'static str {
        match (self, glyphs) {
            (Self::Success, _) => "✓",
            (Self::Warning, Glyphs::Nerd) => "󰀪",
            (Self::Warning, Glyphs::Unicode) => "!",
            (Self::Error, Glyphs::Nerd) => "󰅚",
            (Self::Error, Glyphs::Unicode) => "✗",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Success => GREEN,
            Self::Warning => YELLOW,
            Self::Error => RED,
        }
    }
}

/// `message` as a marked line: its mark, then the message with its later
/// lines set under the first. Where the stream takes no marks it is `plain`
/// and the message, as clanka wrote it before.
pub fn marked(paint: Paint, glyphs: Glyphs, mark: Mark, plain: &str, message: &str) -> String {
    if !paint.marks {
        return format!("{plain}{message}");
    }
    let glyph = paint.paint(mark.style(), mark.glyph(glyphs));
    format!("{glyph} {}", message.replace('\n', "\n  "))
}

fn mark_out(mark: Mark, plain: &str, message: &str) {
    let paint = look().out;
    let glyphs = if paint.marks {
        glyphs()
    } else {
        Glyphs::Unicode
    };
    print(&marked(paint, glyphs, mark, plain, message));
}

fn mark_err(mark: Mark, plain: &str, message: &str) {
    let paint = look().err;
    let glyphs = if paint.marks {
        glyphs()
    } else {
        Glyphs::Unicode
    };
    log(&marked(paint, glyphs, mark, plain, message));
}

/// What a command did, on stdout.
pub fn success(message: &str) {
    mark_out(Mark::Success, "", message);
}

/// Something the user should know went wrong without stopping the command,
/// on stderr. Piped it reads `warning: …`.
pub fn warning(message: &str) {
    mark_err(Mark::Warning, "warning: ", message);
}

/// Something that failed, on stderr. Piped it reads `plain` and the message.
pub fn error(plain: &str, message: &str) {
    mark_err(Mark::Error, plain, message);
}

/// A command's failure, on stderr: what failed, then what caused it. Piped it
/// is what `main` returning the error prints.
pub fn failure(error: &anyhow::Error) {
    let paint = look().err;
    if !paint.marks {
        return log(&format!("Error: {error:?}"));
    }
    let mut message = error.to_string();
    for cause in error.chain().skip(1) {
        message += &format!("\n{}", paint.paint(DIM, &cause.to_string()));
    }
    mark_err(Mark::Error, "", &message);
}

/// One of clanka's own asides, on stderr, dimmed where there is color.
pub fn note(line: &str) {
    log(&look().err.paint(DIM, line));
}

/// The live row. Each step gets a bar of its own, so its time starts at 0.
pub(crate) struct Row {
    target: fn() -> ProgressDrawTarget,
    paint: Paint,
    /// Asked when a row is first drawn: the answer may take a look through
    /// the font directories, which a command with no row never needs.
    glyphs: fn() -> Glyphs,
    bar: Option<ProgressBar>,
    label: String,
    detailed: bool,
}

impl Row {
    pub(crate) fn new(
        target: fn() -> ProgressDrawTarget,
        paint: Paint,
        glyphs: fn() -> Glyphs,
    ) -> Self {
        Self {
            target,
            paint,
            glyphs,
            bar: None,
            label: String::new(),
            detailed: false,
        }
    }

    /// Draws the step, or changes what the row says if it is this step's.
    pub(crate) fn show(&mut self, label: &str, amount: Option<Amount>, detail: Option<&str>) {
        if self.label != label {
            self.clear();
        }
        let detailed = detail.is_some();
        let fresh = self.bar.is_none();
        let bar = self.bar.get_or_insert_with(|| {
            self.label = label.to_owned();
            ProgressBar::with_draw_target(None, (self.target)())
        });
        // The words before the style that shows them: a detail line is never
        // drawn empty.
        bar.set_prefix(header(label, amount, self.paint));
        bar.set_message(detail.unwrap_or_default().to_owned());
        if fresh || detailed != self.detailed {
            self.detailed = detailed;
            bar.set_style(style(detailed, self.paint, (self.glyphs)()));
        }
        if fresh {
            bar.enable_steady_tick(Duration::from_millis(100));
        }
    }

    /// Takes the row off the screen.
    pub(crate) fn clear(&mut self) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
        self.label.clear();
    }

    /// The bar on screen, if one is.
    pub(crate) fn bar(&self) -> Option<ProgressBar> {
        self.bar.clone()
    }
}

/// The row's template: the step and its time, and under it the detail, cut
/// to the terminal's width, as it may be a provider's error. With Nerd Font
/// symbols the row is marked as manifold's and outfitting-manager's are; the
/// time going up says it is alive.
fn template(detailed: bool, paint: Paint, glyphs: Glyphs) -> String {
    let (lead, cog) = match glyphs {
        Glyphs::Nerd => ("󰔛", "󰒓 "),
        Glyphs::Unicode => ("{spinner}", ""),
    };
    let mut template = format!(
        "{} {{prefix}} {}",
        paint.paint(CYAN, lead),
        paint.paint(DIM, "· {time}")
    );
    if detailed {
        template += &format!("\n    {}", paint.paint(DIM, &format!("{cog}{{wide_msg}}")));
    }
    template
}

fn style(detailed: bool, paint: Paint, glyphs: Glyphs) -> ProgressStyle {
    ProgressStyle::with_template(&template(detailed, paint, glyphs))
        .expect("the templates are fixed")
        .tick_strings(SPINNER)
        .with_key(
            "time",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                let seconds = state.elapsed().as_secs();
                let _ = write!(w, "{}:{:02}", seconds / 60, seconds % 60);
            },
        )
}

/// The step's name and how far it is: `Re-reading memories · 40/120 · 33%`.
fn header(label: &str, amount: Option<Amount>, paint: Paint) -> String {
    let mut header = paint.paint(BOLD, label);
    let Some(amount) = amount else {
        return header;
    };
    let Amount { done, total, bytes } = amount;
    let done = if total > 0 { done.min(total) } else { done };
    match (bytes, amount.percent()) {
        (false, Some(percent)) => {
            let _ = write!(header, " · {done}/{total} · {percent}%");
        }
        // A count of no known total says nothing a spinner doesn't.
        (false, None) => {}
        (true, percent) => {
            let (unit, size) = if done.max(total) >= 1_000_000 {
                ("MB", 1e6)
            } else {
                ("kB", 1e3)
            };
            let _ = write!(header, " · {:.1}", done as f64 / size);
            if let Some(percent) = percent {
                let _ = write!(header, "/{:.1} {unit} · {percent}%", total as f64 / size);
            } else {
                let _ = write!(header, " {unit}");
            }
        }
    }
    header
}

/// Writes a line to stderr, above the live row if one is up.
pub fn log(line: &str) {
    status::session().above(|| eprintln!("{line}"));
}

/// Writes a line to stdout, above the live row if one is up.
pub fn print(line: &str) {
    status::session().above(|| println!("{line}"));
}

/// A stream written above the live row: each write takes the row away, writes,
/// and puts it back.
pub struct Above<W>(pub W);

impl<W: Write> Write for Above<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        status::session().above(|| self.0.write(buf))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_says_how_far_the_step_is() {
        let amount = |done, total, bytes| Some(Amount { done, total, bytes });
        assert_eq!(header("Drafting Nova", None, Paint::PLAIN), "Drafting Nova");
        // 40 of 120 is 33.3%: floored, as the status report is.
        assert_eq!(
            header("Re-reading memories", amount(40, 120, false), Paint::PLAIN),
            "Re-reading memories · 40/120 · 33%"
        );
        assert_eq!(
            header("Refreshing", amount(3, 0, false), Paint::PLAIN),
            "Refreshing"
        );
        // More than the whole is the whole.
        assert_eq!(
            header("Rebuilding", amount(9, 4, false), Paint::PLAIN),
            "Rebuilding · 4/4 · 100%"
        );
        assert_eq!(
            header(
                "Downloading model.onnx",
                amount(9_240_000, 23_000_000, true),
                Paint::PLAIN
            ),
            "Downloading model.onnx · 9.2/23.0 MB · 40%"
        );
        // The unit is the whole file's, not what has arrived of it.
        assert_eq!(
            header(
                "Downloading model.onnx",
                amount(512_000, 23_000_000, true),
                Paint::PLAIN
            ),
            "Downloading model.onnx · 0.5/23.0 MB · 2%"
        );
        assert_eq!(
            header(
                "Downloading config.json",
                amount(500, 2_000, true),
                Paint::PLAIN
            ),
            "Downloading config.json · 0.5/2.0 kB · 25%"
        );
        // A server that didn't say how long the file is.
        assert_eq!(
            header(
                "Downloading model.onnx",
                amount(9_240_000, 0, true),
                Paint::PLAIN
            ),
            "Downloading model.onnx · 9.2 MB"
        );
    }

    const COLOR: Paint = Paint {
        marks: true,
        color: true,
    };
    const MARKS: Paint = Paint {
        marks: true,
        color: false,
    };

    #[test]
    fn a_stream_is_marked_on_a_terminal_and_colored_unless_no_color_is_set() {
        assert_eq!(Paint::of(true, Some("xterm-256color"), None), COLOR);
        // An empty NO_COLOR is no NO_COLOR.
        assert_eq!(Paint::of(true, None, Some("")), COLOR);
        assert_eq!(Paint::of(true, Some("xterm-256color"), Some("1")), MARKS);
        assert_eq!(Paint::of(true, Some("dumb"), None), Paint::PLAIN);
        assert_eq!(Paint::of(false, Some("xterm-256color"), None), Paint::PLAIN);

        assert_eq!(COLOR.paint(GREEN, "ok"), "\x1b[32mok\x1b[0m");
        assert_eq!(MARKS.paint(GREEN, "ok"), "ok");
    }

    #[test]
    fn a_piped_line_is_what_it_was_and_a_terminal_gets_the_mark() {
        let line = |paint, glyphs, mark, plain| {
            marked(
                paint,
                glyphs,
                mark,
                plain,
                "KEY is not set\nin ~/.hermes/.env",
            )
        };
        assert_eq!(
            line(Paint::PLAIN, Glyphs::Nerd, Mark::Warning, "warning: "),
            "warning: KEY is not set\nin ~/.hermes/.env"
        );
        // Later lines sit under the first one's text.
        assert_eq!(
            line(MARKS, Glyphs::Unicode, Mark::Warning, "warning: "),
            "! KEY is not set\n  in ~/.hermes/.env"
        );
        assert_eq!(
            line(MARKS, Glyphs::Nerd, Mark::Error, "error: "),
            "\u{f015a} KEY is not set\n  in ~/.hermes/.env"
        );
        assert_eq!(
            marked(COLOR, Glyphs::Nerd, Mark::Success, "", "Created Nova."),
            "\x1b[32m✓\x1b[0m Created Nova."
        );
    }

    #[test]
    fn the_row_is_marked_as_the_glyphs_allow() {
        assert_eq!(
            template(false, Paint::PLAIN, Glyphs::Unicode),
            "{spinner} {prefix} · {time}"
        );
        assert_eq!(
            template(true, Paint::PLAIN, Glyphs::Unicode),
            "{spinner} {prefix} · {time}\n    {wide_msg}"
        );
        assert_eq!(
            template(true, Paint::PLAIN, Glyphs::Nerd),
            "\u{f051b} {prefix} · {time}\n    \u{f0493} {wide_msg}"
        );
        assert_eq!(
            template(false, COLOR, Glyphs::Unicode),
            "\x1b[36m{spinner}\x1b[0m {prefix} \x1b[2m· {time}\x1b[0m"
        );
        assert_eq!(
            header(
                "Rebuilding",
                Some(Amount {
                    done: 1,
                    total: 2,
                    bytes: false
                }),
                COLOR
            ),
            "\x1b[1mRebuilding\x1b[0m · 1/2 · 50%"
        );
    }

    /// An environment of the given variables and nothing else.
    fn env(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn the_setting_decides_when_it_names_a_set() {
        let fonts = tempfile::tempdir().unwrap();
        std::fs::write(fonts.path().join("BlexMonoNerdFont-Regular.ttf"), "").unwrap();
        let dirs = [fonts.path().to_owned()];
        let ghostty = env(&[("TERM", "xterm-ghostty")]);
        assert_eq!(
            detect(GlyphSetting::Unicode, &ghostty, &dirs),
            Glyphs::Unicode
        );
        assert_eq!(detect(GlyphSetting::Nerd, env(&[]), &[]), Glyphs::Nerd);
        assert_eq!(detect(GlyphSetting::Auto, env(&[]), &[]), Glyphs::Unicode);
    }

    #[test]
    fn a_terminal_that_draws_the_symbols_itself_gets_them_anywhere() {
        // Over SSH and with no font here: the terminal has its own.
        for vars in [
            &[
                ("TERM", "xterm-ghostty"),
                ("SSH_CONNECTION", "10.0.0.1 1 10.0.0.2 22"),
            ][..],
            &[("TERM", "xterm-kitty")],
            &[("TERM", "xterm-256color"), ("KITTY_WINDOW_ID", "3")],
            &[("TERM", "xterm-256color"), ("TERM_PROGRAM", "WezTerm")],
            &[("TERM", "xterm-256color"), ("TERM_PROGRAM", "ghostty")],
            &[("TERM", "xterm-rex")],
            &[("TERM", "xterm-256color"), ("REX_SESSION", "s-1")],
        ] {
            let vars: &'static [(&str, &str)] = vars;
            assert_eq!(
                detect(GlyphSetting::Auto, env(vars), &[]),
                Glyphs::Nerd,
                "{vars:?}"
            );
        }
        for vars in [
            &[
                ("TERM", "xterm-256color"),
                ("TERM_PROGRAM", "Apple_Terminal"),
            ][..],
            &[("TERM", "alacritty")],
            &[("TERM", "xterm-256color"), ("REX_SESSION", "")],
        ] {
            let vars: &'static [(&str, &str)] = vars;
            assert_eq!(
                detect(GlyphSetting::Auto, env(vars), &[]),
                Glyphs::Unicode,
                "{vars:?}"
            );
        }
    }

    #[test]
    fn a_nerd_font_installed_here_counts_unless_the_terminal_is_elsewhere() {
        let fonts = tempfile::tempdir().unwrap();
        let dirs = [fonts.path().join("missing"), fonts.path().to_owned()];
        let local = env(&[("TERM", "xterm-256color")]);
        let remote = env(&[
            ("TERM", "xterm-256color"),
            ("SSH_CONNECTION", "10.0.0.1 1 10.0.0.2 22"),
        ]);
        // `monerd` is a font's name, not a Nerd Font.
        std::fs::write(fonts.path().join("monerd_outlinebold.otf"), "").unwrap();
        assert_eq!(detect(GlyphSetting::Auto, &local, &dirs), Glyphs::Unicode);
        // As a package installs one: a directory or two down.
        let nested = fonts.path().join("truetype/blex");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("BlexMonoNerdFont-Regular.ttf"), "").unwrap();
        assert_eq!(detect(GlyphSetting::Auto, &local, &dirs), Glyphs::Nerd);
        // Over SSH the font that draws is on the machine with the screen.
        assert_eq!(detect(GlyphSetting::Auto, &remote, &dirs), Glyphs::Unicode);
    }
}
