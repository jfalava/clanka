//! `clanka debug`: sample program status, for seeing how a terminal shows what
//! the real commands report. Nothing is read, written or asked of a model.

use anyhow::{Result, bail};
use clanka::status::{Outcome, Session, State};
use clap::ValueEnum;
use std::io::Write;
use std::time::Duration;

/// The reports of real commands, on made-up work.
#[derive(Clone, Copy, ValueEnum)]
pub enum Sample {
    /// A step with a percentage, as `embedding` and `doctor reembed` report
    Progress,
    /// Model calls counted under a percentage, one of them waiting on a retry, as `soul
    /// update` reports
    Calls,
    /// A chat: at rest, replying, replied, a reply that fails, and the reflections beside
    /// them, one of which fails
    Chat,
}

/// How a sample run ends.
#[derive(Clone, Copy, ValueEnum)]
pub enum End {
    /// `done`, with a line of what was done
    Done,
    /// `error`, with the reason, and exit status 1
    Error,
    /// `idle`, as a chat that was left
    Idle,
}

const RETRY: &str = "HTTP 529 from the sample provider; trying again in 2.0s";
const REFLECT: &str = "reflect";

/// Plays `sample` on `status`, each report left up for `delay` and named on
/// `out`, so what the terminal shows can be told from what it was sent. `Done`
/// and `Error` leave the last report to `main`, as every command does.
pub fn status(
    status: &Session,
    sample: Sample,
    end: End,
    delay: Duration,
    mut out: impl Write,
) -> Result<()> {
    let mut shown = |what: &str| -> Result<()> {
        writeln!(out, "{what}")?;
        std::thread::sleep(delay);
        Ok(())
    };
    let did = match sample {
        Sample::Progress => {
            status.phase("Downloading model.onnx");
            for tenth in 0..=10 {
                status.progress(tenth, 10);
                shown(&format!("working  Downloading model.onnx, {}%", tenth * 10))?;
            }
            "Sample download finished"
        }
        Sample::Calls => {
            status.phase("Re-reading memories");
            status.count_model_calls();
            for batch in 0..3 {
                status.progress(batch * 20, 60);
                status.model_call();
                shown(&format!(
                    "working  Re-reading memories, {}%, model call {}",
                    batch * 100 / 3,
                    batch + 1
                ))?;
                if batch == 1 {
                    status.retrying(Some(RETRY));
                    shown(&format!("         {RETRY}"))?;
                    status.retrying(None);
                }
            }
            status.progress(60, 60);
            shown("working  Re-reading memories, 100%")?;
            "60 sample memories re-read"
        }
        Sample::Chat => {
            let reflecting = |state, what| status.child(REFLECT, state, "Reflection", what);
            status.report(State::Idle, "Chatting with Nova", None);
            shown("idle     Chatting with Nova")?;
            status.phase("Nova is replying");
            shown("working  Nova is replying")?;
            status.report(State::Done, "Nova replied", None);
            reflecting(State::Working, "Nova is reflecting on turns 1–2");
            shown("done     Nova replied; a reflection begins")?;
            status.phase("Nova is replying");
            shown("working  Nova is replying; the reflection goes on")?;
            status.clear(REFLECT);
            shown("         the reflection ends")?;
            let why = "HTTP 503 from the sample provider";
            status.report(State::Error, "No reply from Nova", Some(why));
            shown(&format!("error    No reply from Nova: {why}"))?;
            status.phase("Nova is replying");
            shown("working  Nova is replying")?;
            status.report(State::Done, "Nova replied", None);
            reflecting(State::Working, "Nova is reflecting on turns 4–5");
            shown("done     Nova replied; a reflection begins")?;
            reflecting(
                State::Error,
                "reflection on turns 4–5 failed: sample failure",
            );
            shown("         the reflection fails, and its record stays")?;
            "Nova is asleep: 3 memories shifted, 2 unresolved things weigh more."
        }
    };
    match end {
        End::Done => {
            status.summary(did);
            writeln!(out, "done     {did}")?;
        }
        End::Error => bail!("sample failure; nothing was run"),
        End::Idle => {
            status.close(Outcome::Idle);
            writeln!(out, "idle     Stopped")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// One OSC 7501 report: its state, its id (`None` for the root), its
    /// message and its percentage.
    type Report = (String, Option<String>, String, Option<String>);

    /// Plays a sample with no delay, then ends the run as `main` does, and
    /// returns what the terminal was sent and what was printed.
    fn play(sample: Sample, end: End) -> (Vec<Report>, String) {
        let (sent, mut printed) = (Buffer::default(), Vec::new());
        let session = Session::to(sent.clone());
        match status(&session, sample, end, Duration::ZERO, &mut printed) {
            Ok(()) => session.close(Outcome::Done),
            Err(error) => session.close(Outcome::Error(error.to_string())),
        }
        let sent = String::from_utf8(sent.0.lock().unwrap().clone()).unwrap();
        let reports = sent
            .split("\x1b\\")
            .filter_map(|sequence| sequence.strip_prefix("\x1b]7501;"))
            .map(|report| {
                let value = |key: &str| {
                    report
                        .split(':')
                        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
                        .map(str::to_owned)
                };
                let msg = value("msg").map(|msg| STANDARD.decode(msg).unwrap());
                (
                    value("state").unwrap(),
                    value("id"),
                    String::from_utf8(msg.unwrap_or_default()).unwrap(),
                    value("progress"),
                )
            })
            .collect();
        (reports, String::from_utf8(printed).unwrap())
    }

    #[test]
    fn progress_counts_up_and_ends_done_with_what_was_done() {
        let (reports, printed) = play(Sample::Progress, End::Done);
        let percents: Vec<&str> = reports.iter().filter_map(|r| r.3.as_deref()).collect();
        assert_eq!(
            percents,
            [
                "0", "10", "20", "30", "40", "50", "60", "70", "80", "90", "100"
            ]
        );
        let (last, working) = reports.split_last().unwrap();
        assert!(working.iter().all(|r| r.0 == "working"), "{working:?}");
        assert_eq!(
            (last.0.as_str(), last.2.as_str()),
            ("done", "Sample download finished")
        );
        assert!(
            printed.ends_with("done     Sample download finished\n"),
            "{printed}"
        );
    }

    #[test]
    fn calls_show_the_retry_over_the_call_count_and_can_end_in_error() {
        let (reports, _) = play(Sample::Calls, End::Error);
        let mut said: Vec<&str> = reports.iter().map(|r| r.2.as_str()).collect();
        said.dedup();
        assert_eq!(
            said,
            [
                "Re-reading memories",
                "model call 1",
                "model call 2",
                RETRY,
                "model call 2",
                "model call 3",
                "sample failure; nothing was run"
            ]
        );
        assert_eq!(reports.last().unwrap().0, "error");
        assert_eq!(reports[reports.len() - 2].3.as_deref(), Some("100"));
    }

    #[test]
    fn chat_reports_replies_beside_its_reflections_and_can_end_idle() {
        let (reports, printed) = play(Sample::Chat, End::Idle);
        let states = |reflection: bool| -> Vec<&str> {
            reports
                .iter()
                .filter(|r| r.1.is_some() == reflection)
                .map(|r| r.0.as_str())
                .collect()
        };
        assert_eq!(
            states(false),
            [
                "idle", "working", "done", "working", "error", "working", "done", "idle"
            ]
        );
        assert_eq!(states(true), ["working", "clear", "working", "error"]);
        assert!(
            reports
                .iter()
                .all(|r| r.1.is_none() || r.1.as_deref() == Some(REFLECT))
        );
        // Leaving closed the run: `main`'s own close adds nothing.
        assert_eq!(reports.last().unwrap().2, "Stopped");
        assert!(printed.ends_with("idle     Stopped\n"), "{printed}");
    }
}
