//! What a person watching the terminal sees while clanka works: one live row
//! on stderr, a spinner beside the step's name with how far it is and how long
//! it has taken, and a line of detail under it. The run's
//! [`Session`](crate::status::Session) draws it from the same calls that
//! report program status, so the two never disagree.
//!
//! The row is there only when stderr is a terminal. Piped, clanka writes what
//! it always wrote.

use crate::status::{self, Amount};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use std::fmt::Write as _;
use std::io::Write;
use std::time::Duration;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "];
const HEADER: &str = "{spinner} {prefix} · {time}";
/// The detail is cut to the terminal's width: it may be a provider's error.
const DETAILED: &str = "{spinner} {prefix} · {time}\n    {wide_msg}";

/// The live row. Each step gets a bar of its own, so its time starts at 0.
pub(crate) struct Row {
    target: fn() -> ProgressDrawTarget,
    bar: Option<ProgressBar>,
    label: String,
    detailed: bool,
}

impl Row {
    pub(crate) fn new(target: fn() -> ProgressDrawTarget) -> Self {
        Self {
            target,
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
        if fresh || detailed != self.detailed {
            self.detailed = detailed;
            bar.set_style(style(detailed));
        }
        bar.set_prefix(header(label, amount));
        bar.set_message(detail.unwrap_or_default().to_owned());
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

fn style(detailed: bool) -> ProgressStyle {
    ProgressStyle::with_template(if detailed { DETAILED } else { HEADER })
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
fn header(label: &str, amount: Option<Amount>) -> String {
    let Some(amount) = amount else {
        return label.to_owned();
    };
    let Amount { done, total, bytes } = amount;
    let done = if total > 0 { done.min(total) } else { done };
    let mut header = label.to_owned();
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
        assert_eq!(header("Drafting Nova", None), "Drafting Nova");
        // 40 of 120 is 33.3%: floored, as the status report is.
        assert_eq!(
            header("Re-reading memories", amount(40, 120, false)),
            "Re-reading memories · 40/120 · 33%"
        );
        assert_eq!(header("Refreshing", amount(3, 0, false)), "Refreshing");
        // More than the whole is the whole.
        assert_eq!(
            header("Rebuilding", amount(9, 4, false)),
            "Rebuilding · 4/4 · 100%"
        );
        assert_eq!(
            header(
                "Downloading model.onnx",
                amount(9_240_000, 23_000_000, true)
            ),
            "Downloading model.onnx · 9.2/23.0 MB · 40%"
        );
        // The unit is the whole file's, not what has arrived of it.
        assert_eq!(
            header("Downloading model.onnx", amount(512_000, 23_000_000, true)),
            "Downloading model.onnx · 0.5/23.0 MB · 2%"
        );
        assert_eq!(
            header("Downloading config.json", amount(500, 2_000, true)),
            "Downloading config.json · 0.5/2.0 kB · 25%"
        );
        // A server that didn't say how long the file is.
        assert_eq!(
            header("Downloading model.onnx", amount(9_240_000, 0, true)),
            "Downloading model.onnx · 9.2 MB"
        );
    }
}
