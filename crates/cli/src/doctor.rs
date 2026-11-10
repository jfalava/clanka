//! `clanka doctor`: the tools for when a companion's memory goes wrong. Look
//! with `log` and `check-facts`, freeze, then repair with `rollback`, `delete`
//! or `reinterpret`; `reembed` moves a companion to another embedding model.

use anyhow::{Context, Result, bail};
use clanka::setup::{self, Config, Paths, SharedEmbedder};
use clanka_core::context::date;
use clap::Subcommand;
use std::path::{Path, PathBuf};

use crate::SOUL_HELP;

const WHEN_HELP: &str = "A time in UTC as `clanka doctor log` prints it (2026-03-09T14:30:05, \
    2026-03-09T14:30 or 2026-03-09), or how long ago: 45m, 6h, 2d";

#[derive(Subcommand)]
pub enum DoctorCommand {
    /// Stop every write to the memory while the doctor works.
    ///
    /// Takes effect at once in every process that has the companion open, and nothing
    /// said meanwhile is recorded or remembered: `clanka chat` stops, a Hermes turn is
    /// answered with a note that memory is frozen instead of by the model, and an MCP
    /// host, which calls its model itself, is told to answer without memory. Sending the
    /// soul's doctor_token in a chat does the same. Lasts until `clanka doctor unfreeze`.
    Freeze {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Let writes through again.
    ///
    /// Lifts a freeze from `clanka doctor freeze` or from the doctor token.
    Unfreeze {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Print every write to the memory since a point in time, oldest first.
    ///
    /// One line per write: when (UTC), who (writer, sleep, doctor, cli), what, and the
    /// details. This is where you find the time to give `clanka doctor rollback`. Reads the
    /// log and changes nothing.
    #[command(after_long_help = "Example:\n  \
        clanka doctor log ~/.config/clanka/souls/<id>.md --since 2d")]
    Log {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        #[arg(long, default_value = "1d", value_name = "WHEN", help = WHEN_HELP)]
        since: String,
    },
    /// Put the memory back as it was at a point in time. Refuses unless frozen.
    ///
    /// Undoes every write logged at that time or later: memories written since are removed,
    /// memories changed since get their earlier text and level back, threads resolved since
    /// are open again, and the mood, notes and soul sections return to what they were.
    /// Seared memories are never touched, and the transcript and the write log are kept, so
    /// nothing said is lost. The exchanges are not reflected on again. Check what will go
    /// with `clanka doctor log --since` the same time first; there is no undo.
    #[command(after_long_help = "Example:\n  \
        clanka doctor freeze ~/.config/clanka/souls/<id>.md\n  \
        clanka doctor log ~/.config/clanka/souls/<id>.md --since 6h\n  \
        clanka doctor rollback ~/.config/clanka/souls/<id>.md --to 2026-03-09T14:30:05\n  \
        clanka doctor unfreeze ~/.config/clanka/souls/<id>.md")]
    Rollback {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        #[arg(long, value_name = "WHEN", help = WHEN_HELP)]
        to: String,
    },
    /// List memories whose facts were never said in the turns they cite.
    ///
    /// A memory's facts (names, dates, places) must appear word for word in the turns it was
    /// written from. This lists every active memory with a fact that does not, or with facts
    /// and no turns to check them against: candidates for `clanka doctor delete` or
    /// `reinterpret`. Reads the memory and changes nothing.
    #[command(after_long_help = "Example:\n  \
        clanka doctor check-facts ~/.config/clanka/souls/<id>.md")]
    CheckFacts {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Have the companion re-read what was said and rewrite these memories. Refuses unless
    /// frozen.
    ///
    /// Needs the [model] in ~/.config/clanka/config.toml. For each memory the model gets
    /// the turns it was written from and writes how the companion sees it now; the old text
    /// stays in the memory's history. Seared memories and memories that cite no turns are
    /// refused. IDs are the #numbers `clanka memories` prints.
    #[command(after_long_help = "Example:\n  \
        clanka doctor reinterpret ~/.config/clanka/souls/<id>.md 12 15")]
    Reinterpret {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        /// The memories to re-read, as `clanka memories` numbers them.
        #[arg(required = true, value_name = "ID")]
        ids: Vec<i64>,
    },
    /// Delete memories for good, with the turns only they cite.
    ///
    /// Irreversible. Removes each memory, its history and the turns no other memory cites,
    /// so what was said is gone from the transcript too. Works on seared memories, frozen
    /// or not. If a memory had replaced an older one, the older one is current again. IDs
    /// are the #numbers `clanka memories` prints.
    #[command(after_long_help = "Example:\n  \
        clanka doctor delete ~/.config/clanka/souls/<id>.md 12")]
    Delete {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        /// The memories to delete, as `clanka memories` numbers them.
        #[arg(required = true, value_name = "ID")]
        ids: Vec<i64>,
    },
    /// Rebuild every memory's vector with the embedding model in config.toml.
    ///
    /// For after changing `[embedding] model`: the companion refuses to open until its
    /// vectors come from the configured model. All vectors are replaced at once or not at
    /// all, a few seconds per thousand memories. A process that had the companion open (a
    /// Hermes gateway, `clanka mcp`) still holds the old model; its turns fail until you
    /// restart it. Does the work again, harmlessly, if the model did not change.
    #[command(
        after_long_help = "Example, after setting [embedding] model = \"multilingual\":\n  \
        clanka embedding\n  \
        clanka doctor reembed ~/.config/clanka/souls/<id>.md"
    )]
    Reembed {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Delete every stored memory, seared ones included. Refuses unless frozen.
    ///
    /// Irreversible. Deletes every memory, the whole transcript, the mood, the soul
    /// sections the companion wrote and every session, so the companion starts blank. The
    /// soul.md, the database key and the write log, which records only that deletes
    /// happened, are kept. If some memories refuse to delete, it lists them, keeps the
    /// transcript and sessions and exits non-zero; run it again once they are fixed. The
    /// companion stays frozen afterwards.
    DeleteAll {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
}

pub fn run(paths: &Paths, command: DoctorCommand) -> Result<()> {
    // The doctor opens the companion without a model, except to re-read memories.
    let open = |soul: &Path| setup::open(paths, soul, SharedEmbedder::load(paths)?, None);
    match command {
        DoctorCommand::Freeze { soul } => {
            open(&soul)?.set_frozen(true)?;
            println!(
                "Frozen {}. Nothing writes to its memory until you unfreeze.",
                soul.display()
            );
        }
        DoctorCommand::Unfreeze { soul } => {
            open(&soul)?.set_frozen(false)?;
            println!("Unfrozen {}. Writes go through again.", soul.display());
        }
        DoctorCommand::Log { soul, since } => {
            let since = when(&since, now())?;
            let entries = open(&soul)?.writes_since(since - 1)?;
            for e in &entries {
                let target = match e.target_id {
                    Some(id) => format!("{} #{id}", e.target),
                    None => e.target.clone(),
                };
                println!(
                    "{}  {}  {}  {target}  {}",
                    date_time(e.created_at),
                    e.actor,
                    e.action,
                    e.payload
                );
            }
            println!("{} writes since {} UTC.", entries.len(), date_time(since));
        }
        DoctorCommand::Rollback { soul, to } => {
            let to = when(&to, now())?;
            let report = open(&soul)?.rollback_to(to - 1)?;
            println!(
                "Rolled {} back to {} UTC: {} memories removed, {} restored to their earlier \
                 text, {} threads reopened, {} mood, note and section entries dropped.",
                soul.display(),
                date_time(to),
                report.removed.len(),
                report.restored.len(),
                report.reopened.len(),
                report.state_rows
            );
            if !report.kept_seared.is_empty() {
                println!(
                    "Seared memories are never rolled back; left as they are: {}.",
                    ids(&report.kept_seared)
                );
            }
        }
        DoctorCommand::CheckFacts { soul } => {
            let issues = open(&soul)?.check_facts()?;
            for issue in &issues {
                match &issue.fact {
                    Some((key, value)) => println!(
                        "#{}: {key} = {value:?} was not said in the turns it cites",
                        issue.memory
                    ),
                    None => println!("#{}: has facts but cites no turns", issue.memory),
                }
            }
            if issues.is_empty() {
                println!("Every fact was said in the turns its memory cites.");
            }
        }
        DoctorCommand::Reinterpret { soul, ids } => {
            let config = Config::load(paths)?;
            let Some(model) = &config.model else {
                bail!(
                    "no companion model configured to re-read the memories; add a [model] \
                     section to {}",
                    paths.config_file().display()
                );
            };
            let mut companion =
                setup::open(paths, &soul, SharedEmbedder::load(paths)?, Some(model))?;
            let reflection = companion.reinterpret(&ids)?;
            let mut failed = 0;
            for call in &reflection.calls {
                if !call.ok {
                    failed += 1;
                    println!("  {}: {}", call.name, call.result);
                }
            }
            let done = reflection
                .calls
                .iter()
                .filter(|c| c.ok && c.name == "reinterpret")
                .count();
            println!(
                "Re-read {done} of {} memories in {} model calls.",
                ids.len(),
                reflection.rounds
            );
            if done < ids.len() {
                bail!(
                    "{} memories were not rewritten ({failed} refused calls); run it again \
                     for those",
                    ids.len() - done
                );
            }
        }
        DoctorCommand::Delete { soul, ids: doomed } => {
            let mut companion = open(&soul)?;
            for id in doomed {
                let deleted = companion
                    .delete_memory(id)
                    .with_context(|| format!("deleting memory {id}"))?;
                let restored = match deleted.restored {
                    Some(old) => format!("; #{old}, which it had replaced, is current again"),
                    None => String::new(),
                };
                println!(
                    "Deleted #{id} and {} turns only it cited{restored}.",
                    deleted.turns.len()
                );
            }
        }
        DoctorCommand::Reembed { soul } => {
            let embedder = SharedEmbedder::load(paths)?;
            let companion = setup::reembed(paths, &soul, embedder)?;
            println!(
                "Rebuilt the vectors of {} with {}. Restart anything that has it open: until \
                 then its turns fail instead of mixing the two models.",
                soul.display(),
                companion.deps().embedder.model_id()
            );
        }
        DoctorCommand::DeleteAll { soul } => delete_all(open(&soul)?, &soul)?,
    }
    Ok(())
}

fn delete_all(mut companion: clanka_core::Companion, soul: &Path) -> Result<()> {
    if !companion.is_frozen()? {
        bail!(
            "{} is not frozen; run `clanka doctor freeze {}` first, so nothing writes \
             to the memory while it is deleted",
            soul.display(),
            soul.display()
        );
    }
    let report = companion.delete_all_memory()?;
    if report.failed.is_empty() {
        println!(
            "Deleted {} memories and {} turns, then {} leftover turns and {} sessions, \
             from {}.",
            report.memories,
            report.turns,
            report.leftover_turns,
            report.sessions,
            soul.display()
        );
        return Ok(());
    }
    println!(
        "Deleted {} memories and {} turns from {}.",
        report.memories,
        report.turns,
        soul.display()
    );
    for (id, why) in &report.failed {
        println!("  #{id}: {why}");
    }
    bail!(
        "{} memories failed to delete, so the transcript and sessions were kept; \
         fix the failures and run `clanka doctor delete-all {}` again",
        report.failed.len(),
        soul.display()
    );
}

fn ids(ids: &[i64]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn now() -> i64 {
    use clanka_core::deps::{Clock, SystemClock};
    SystemClock.now_ms()
}

const SECOND: i64 = 1000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// A timestamp as the log prints it and `when` reads it back: UTC, to the second.
fn date_time(ms: i64) -> String {
    let of_day = ms.rem_euclid(DAY);
    format!(
        "{}T{:02}:{:02}:{:02}",
        date(ms),
        of_day / HOUR,
        of_day % HOUR / MINUTE,
        of_day % MINUTE / SECOND
    )
}

/// Milliseconds since the epoch for a `WHEN` argument: `45m`, `6h` or `2d`
/// before `now`, or a UTC date with an optional time.
fn when(text: &str, now: i64) -> Result<i64> {
    let text = text.trim();
    let bad =
        || format!("{text:?} is not a time: use 2026-03-09T14:30:05, 2026-03-09 or an age like 6h");
    if let Some((unit, amount)) = text
        .char_indices()
        .last()
        .map(|(i, unit)| (unit, &text[..i]))
        && let Some(unit) = match unit {
            'm' => Some(MINUTE),
            'h' => Some(HOUR),
            'd' => Some(DAY),
            _ => None,
        }
    {
        let amount: i64 = amount.parse().ok().filter(|n| *n >= 0).with_context(bad)?;
        return Ok(now - amount.checked_mul(unit).with_context(bad)?);
    }

    let (day, time) = match text.split_once(['T', ' ']) {
        Some((day, time)) => (day, time),
        None => (text, "00:00:00"),
    };
    let numbers = |text: &str, separator: char| -> Option<Vec<i64>> {
        text.split(separator)
            .map(|part| part.parse::<i64>().ok().filter(|n| *n >= 0))
            .collect()
    };
    let day = numbers(day, '-').with_context(bad)?;
    let time = numbers(time, ':').with_context(bad)?;
    let (&[year, month, day], [hour, minute, rest @ ..]) = (&day[..], &time[..]) else {
        bail!(bad());
    };
    let second = match rest {
        [] => 0,
        [second] => *second,
        _ => bail!(bad()),
    };
    // Past year 9999 the arithmetic below would overflow.
    if !(0..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || *hour > 23
        || *minute > 59
        || second > 59
    {
        bail!(bad());
    }
    // A civil date to days since the epoch (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms = days * DAY + hour * HOUR + minute * MINUTE + second * SECOND;
    // 2026-02-31 is not a day: it must read back as itself.
    if date(ms) != format!("{year:04}-{month:02}-{day:02}") {
        bail!(bad());
    }
    Ok(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_the_log_prints_reads_back_as_itself() {
        let now = 1_794_268_800_000 + 14 * HOUR + 30 * MINUTE + 5 * SECOND + 123;
        assert_eq!(date_time(now), "2026-11-10T14:30:05");
        assert_eq!(when(&date_time(now), 0).unwrap(), now - 123);
        assert_eq!(when("2026-11-10", 0).unwrap(), 1_794_268_800_000);
        assert_eq!(
            when("2026-11-10 14:30", 0).unwrap(),
            1_794_268_800_000 + 14 * HOUR + 30 * MINUTE
        );
        assert_eq!(when("2024-02-29", 0).unwrap(), 1_709_164_800_000);
        assert_eq!(when("1970-01-01T00:00:00", 0).unwrap(), 0);

        assert_eq!(when("45m", now).unwrap(), now - 45 * MINUTE);
        assert_eq!(when("6h", now).unwrap(), now - 6 * HOUR);
        assert_eq!(when("2d", now).unwrap(), now - 2 * DAY);

        for bad in [
            "",
            "soon",
            "d",
            "-2d",
            "2w",
            "2026-02-31",
            "2025-02-29",
            "2026-13-01",
            "2026-11-10T24:00",
            "2026-11-10T10",
            "2026-11",
            "10:30",
        ] {
            assert!(when(bad, now).is_err(), "{bad:?} was read as a time");
        }
    }
}
