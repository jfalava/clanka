//! `clanka`: set up companions, check their souls and serve them over MCP.

use anyhow::{Context, Result, bail};
use clanka::setup::{
    self, Config, MODEL_FILES, MODEL_REPO, Paths, SecretServiceKeys, SharedEmbedder,
};
use clanka_core::PersonaStatus;
use clanka_core::soul::Soul;
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "clanka",
    version,
    about = "Long-term memory for chat companions",
    long_about = "Long-term memory for chat companions.\n\n\
        A companion is a soul.md (its persona and traits) plus an encrypted database of what \
        it remembers. Start with `clanka init`, edit the soul.md it prints, then talk with \
        the companion through `clanka chat` or serve it to an MCP host with `clanka mcp`.",
    after_long_help = "Files (Linux):\n  \
        ~/.config/clanka/souls/<id>.md   each companion's soul.md\n  \
        ~/.config/clanka/config.toml     the [model] chat and memory writing run on\n  \
        ~/.local/share/clanka/           the databases and the embedding model\n\n\
        Every command explains itself: `clanka help <command>`, `clanka <command> help` or \
        `clanka <command> --help`."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new companion: a soul.md to edit, its database key and the embedding model.
    ///
    /// Writes a soul.md template under ~/.config/clanka/souls/ with a fresh ID and doctor
    /// token, stores a random database key in the Secret Service (service "clanka") and
    /// downloads the embedding model if it is missing. The database itself is created the
    /// first time the companion opens. Prints the soul.md path that every other command
    /// takes.
    #[command(after_long_help = "Example:\n  clanka init Nova")]
    Init {
        /// The companion's display name.
        name: String,
    },
    /// Check or print a soul.md, or settle a change to its persona.
    ///
    /// The persona is the soul.md's Markdown body: who the companion is. Editing it makes
    /// chat, MCP and Hermes refuse the companion until you choose what its memories do:
    /// `continue`, `update` or `fork`. Editing the frontmatter (traits such as drift or
    /// tired_at, or the doctor token) is not a persona change; it applies the next time the
    /// companion opens. Either way, restart anything that has the companion open.
    #[command(after_long_help = "Example, after editing the persona:\n  \
        clanka soul lint ~/.config/clanka/souls/<id>.md\n  \
        clanka soul continue ~/.config/clanka/souls/<id>.md\n  \
        clanka soul persona ~/.config/clanka/souls/<id>.md > SOUL.md   # for Hermes")]
    Soul {
        #[command(subcommand)]
        command: SoulCommand,
    },
    /// The doctor's tools for when a companion's memory goes wrong.
    ///
    /// Freeze the companion first: while it is frozen nothing writes to its memory, and the
    /// doctor's destructive commands refuse to run unless it is. Unfreeze when you are done.
    #[command(after_long_help = "Example:\n  \
        clanka doctor freeze ~/.config/clanka/souls/<id>.md\n  \
        clanka doctor delete-all ~/.config/clanka/souls/<id>.md\n  \
        clanka doctor unfreeze ~/.config/clanka/souls/<id>.md")]
    Doctor {
        #[command(subcommand)]
        command: DoctorCommand,
    },
    /// Talk with a companion in the terminal, on its [model].
    ///
    /// Needs a [model] section in ~/.config/clanka/config.toml (base_url, model and
    /// api_key_env, which names the environment variable holding the key). Type a message
    /// and press Enter; each reply is remembered in the background. Send /sleep to end the
    /// conversation and turn it into memories, or send the soul's doctor_token to freeze
    /// the memory and quit without recording anything. Refuses to start if the persona
    /// changed since its memories were written.
    #[command(after_long_help = "Example:\n  clanka chat ~/.config/clanka/souls/<id>.md")]
    Chat {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        /// Estimated tokens the memory context may use per turn.
        #[arg(long, default_value_t = 6000)]
        budget: usize,
    },
    /// Serve a companion to an MCP host over stdio, for trying it out.
    ///
    /// The host does the talking and calls the tools: begin_turn before each reply, end_turn
    /// after it, recall to search memory on purpose, and sleep to end the conversation. The
    /// persona prompt tells the host's model how to use them. Memory is written by the
    /// [model] in ~/.config/clanka/config.toml, so set the host to the same model; without
    /// one, turns are recorded but never turned into memories and sleep is unavailable.
    /// stdout carries the MCP protocol; notes go to stderr.
    #[command(after_long_help = "Example, as an MCP server command:\n  \
        clanka mcp ~/.config/clanka/souls/<id>.md")]
    Mcp {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        /// Estimated tokens the memory context may use per turn.
        #[arg(long, default_value_t = 6000)]
        budget: usize,
    },
}

const SOUL_HELP: &str = "The companion's soul.md, as printed by `clanka init`";

#[derive(Subcommand)]
enum SoulCommand {
    /// Check a soul.md for errors.
    ///
    /// Fails if the frontmatter or the persona does not parse. Warns, without failing, when
    /// there are no core values, when a core value is too vague to defend, or when the
    /// persona is long enough to crowd every turn. Changes nothing.
    #[command(after_long_help = "Example:\n  clanka soul lint ~/.config/clanka/souls/<id>.md")]
    Lint {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Print the persona without its frontmatter, for a host's own persona file.
    ///
    /// Use it to fill a persona file such as Hermes's SOUL.md. The frontmatter is left out
    /// because it holds the doctor token, which must never reach the model. Changes nothing.
    #[command(
        after_long_help = "Example:\n  clanka soul persona ~/.config/clanka/souls/<id>.md > SOUL.md"
    )]
    Persona {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Accept an edited persona and keep every memory as it was felt.
    ///
    /// Old memories stay as they were written under the old persona; new ones are written
    /// under the new one. Instant, and calls no model. Does nothing if the persona did not
    /// change.
    #[command(after_long_help = "Example:\n  clanka soul continue ~/.config/clanka/souls/<id>.md")]
    Continue {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Accept an edited persona and have the companion re-read its memories under it.
    ///
    /// Needs the [model] in ~/.config/clanka/config.toml. Freezes writes, accepts the
    /// persona, then asks the model to re-interpret every active memory that cites what was
    /// said, in batches, from the transcript. Seared memories never change. Unfreezes when
    /// it is done, unless the companion was frozen before. If it fails, the persona is
    /// already accepted and the companion stays frozen: run it again to retry, or `clanka
    /// doctor unfreeze` to keep the memories as they are. Takes a model call per batch.
    /// Does nothing if the persona did not change and the companion is not frozen.
    #[command(after_long_help = "Example:\n  clanka soul update ~/.config/clanka/souls/<id>.md")]
    Update {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Start a new companion from this soul.md and archive the old one.
    ///
    /// Archives the companion's database, which keeps everything but becomes read-only for
    /// good, then gives the soul.md a new ID and a new database key in the Secret Service.
    /// The next open starts the new companion with no memories, under the soul.md as it is
    /// now. Nothing is deleted. Works whether or not the persona changed; if it stops
    /// halfway, run it again.
    #[command(after_long_help = "Example:\n  clanka soul fork ~/.config/clanka/souls/<id>.md")]
    Fork {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
}

#[derive(Subcommand)]
enum DoctorCommand {
    /// Stop every write to the memory while the doctor works.
    ///
    /// Takes effect at once in every process that has the companion open: chat, MCP and
    /// Hermes turns fail as frozen instead of being recorded. Sending the soul's
    /// doctor_token in a chat does the same. Lasts until `clanka doctor unfreeze`.
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

/// `clanka <command> help` prints that command's help on every command, not
/// only on those with subcommands, so `clanka init help` explains init instead
/// of creating a companion named "help".
fn args(mut args: Vec<OsString>) -> Vec<OsString> {
    // `clanka help ...` is clap's own and already names a command.
    if args.len() > 2
        && args[1] != "help"
        && let Some(last) = args.last_mut()
        && last == "help"
    {
        *last = "--help".into();
    }
    args
}

fn main() -> Result<()> {
    let paths = Paths::get()?;
    match Cli::parse_from(args(std::env::args_os().collect())).command {
        Command::Init { name } => init(&paths, &name),
        Command::Soul {
            command: SoulCommand::Lint { soul },
        } => lint(&soul),
        Command::Soul {
            command: SoulCommand::Persona { soul },
        } => {
            println!("{}", Soul::load(&soul)?.body.trim());
            Ok(())
        }
        Command::Soul {
            command: SoulCommand::Continue { soul },
        } => persona_continue(&paths, &soul),
        Command::Soul {
            command: SoulCommand::Update { soul },
        } => persona_update(&paths, &soul),
        Command::Soul {
            command: SoulCommand::Fork { soul },
        } => fork(&paths, &soul),
        Command::Doctor {
            command: DoctorCommand::Freeze { soul },
        } => doctor(&paths, &soul, DoctorAction::Freeze),
        Command::Doctor {
            command: DoctorCommand::Unfreeze { soul },
        } => doctor(&paths, &soul, DoctorAction::Unfreeze),
        Command::Doctor {
            command: DoctorCommand::DeleteAll { soul },
        } => doctor(&paths, &soul, DoctorAction::DeleteAll),
        Command::Chat { soul, budget } => {
            let config = Config::load(&paths)?;
            if config.model.is_none() {
                bail!(
                    "no companion model configured; add a [model] section to {}",
                    paths.config_file().display()
                );
            }
            let (chat, writer) = open_pair(&paths, &soul, &config)?;
            clanka::chat::run(chat, writer.expect("a model is configured"), budget)
        }
        Command::Mcp { soul, budget } => {
            let config = Config::load(&paths)?;
            match &config.model {
                // stdout is the MCP channel; notes go to stderr.
                Some(m) => eprintln!(
                    "clanka: memory is written by {} at {}. Set your MCP host to chat with the \
                     same model; clanka can't see which one it uses.",
                    m.model, m.base_url
                ),
                None => eprintln!(
                    "clanka: no [model] in {}, so turns are recorded but never reflected on and \
                     sleep is unavailable.",
                    paths.config_file().display()
                ),
            }
            let (chat, writer) = open_pair(&paths, &soul, &config)?;
            clanka::mcp::serve(chat, writer, budget)
        }
    }
}

/// Two handles to one companion on the same model: one for the hot path and
/// one for reflection and sleep, so a slow reflection never blocks a turn.
/// The second is `None` without a model.
fn open_pair(
    paths: &Paths,
    soul: &Path,
    config: &Config,
) -> Result<(clanka_core::Companion, Option<clanka_core::Companion>)> {
    let embedder = SharedEmbedder::load(paths)?;
    let chat = setup::open(paths, soul, embedder.clone(), config.model.as_ref())?;
    if let PersonaStatus::Changed { .. } = chat.persona_status() {
        bail!(
            "{} changed since its memories were written; run `clanka soul continue`, `update` or \
             `fork` on it first (`clanka soul help` explains each)",
            soul.display()
        );
    }
    let writer = match &config.model {
        Some(m) => Some(setup::open(paths, soul, embedder, Some(m))?),
        None => None,
    };
    Ok((chat, writer))
}

enum DoctorAction {
    Freeze,
    Unfreeze,
    DeleteAll,
}

/// The doctor opens the companion without a model: none of its work calls one.
fn doctor(paths: &Paths, soul: &Path, action: DoctorAction) -> Result<()> {
    let embedder = SharedEmbedder::load(paths)?;
    let mut companion = setup::open(paths, soul, embedder, None)?;
    match action {
        DoctorAction::Freeze => {
            companion.set_frozen(true)?;
            println!(
                "Frozen {}. Nothing writes to its memory until you unfreeze.",
                soul.display()
            );
        }
        DoctorAction::Unfreeze => {
            companion.set_frozen(false)?;
            println!("Unfrozen {}. Writes go through again.", soul.display());
        }
        DoctorAction::DeleteAll => {
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
    }
    Ok(())
}

/// The persona choices open the companion as the doctor does, without the
/// refusal `open_pair` gives a changed persona: settling that is their job.
fn persona_continue(paths: &Paths, soul: &Path) -> Result<()> {
    let mut companion = setup::open(paths, soul, SharedEmbedder::load(paths)?, None)?;
    refuse_archived(&companion, soul)?;
    if !matches!(companion.persona_status(), PersonaStatus::Changed { .. }) {
        println!(
            "{}: the persona has not changed; nothing to do.",
            soul.display()
        );
        return Ok(());
    }
    companion.continue_with_persona()?;
    println!(
        "Continued {} under its new persona; its memories stay as they were felt. Restart \
         anything that has it open.",
        soul.display()
    );
    Ok(())
}

fn persona_update(paths: &Paths, soul: &Path) -> Result<()> {
    let config = Config::load(paths)?;
    let Some(model) = &config.model else {
        bail!(
            "no companion model configured to re-read the memories; add a [model] section to {}, \
             or use `clanka soul continue`",
            paths.config_file().display()
        );
    };
    let mut companion = setup::open(paths, soul, SharedEmbedder::load(paths)?, Some(model))?;
    refuse_archived(&companion, soul)?;
    // A failed update has already accepted the persona and left it frozen; that
    // is the one way to need an update with the persona unchanged.
    if !matches!(companion.persona_status(), PersonaStatus::Changed { .. })
        && !companion.is_frozen()?
    {
        println!(
            "{}: the persona has not changed; nothing to do.",
            soul.display()
        );
        return Ok(());
    }
    let reflections = companion.update_persona()?;
    let reinterpreted = reflections
        .iter()
        .flat_map(|r| &r.calls)
        .filter(|c| c.name == "reinterpret" && c.ok)
        .count();
    let rounds: usize = reflections.iter().map(|r| r.rounds).sum();
    println!(
        "Updated {}: {reinterpreted} memories re-read under the new persona in {rounds} model \
         calls. Restart anything that has it open.",
        soul.display()
    );
    if companion.is_frozen()? {
        println!(
            "It is still frozen, as it was before; run `clanka doctor unfreeze {}` when ready.",
            soul.display()
        );
    }
    Ok(())
}

fn refuse_archived(companion: &clanka_core::Companion, soul: &Path) -> Result<()> {
    if companion.is_archived()? {
        bail!(
            "{} points at an archived companion; run `clanka soul fork {}` to finish forking it",
            soul.display(),
            soul.display()
        );
    }
    Ok(())
}

/// Archives the companion, then moves the soul.md to a new ID and key. The key
/// is stored first and the file replaced last, so stopping anywhere leaves
/// either the old companion or an archived one that another fork finishes.
fn fork(paths: &Paths, soul: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(soul).with_context(|| format!("reading {}", soul.display()))?;
    let old = Soul::parse(&text)?.traits.id;
    let new = uuid::Uuid::now_v7();
    let forked = with_id(&text, old, new)?;
    let mut companion = setup::open(paths, soul, SharedEmbedder::load(paths)?, None)?;
    SecretServiceKeys::store(new, &setup::random_hex(32))?;
    companion.archive()?;
    drop(companion);
    let partial = soul.with_extension("md.part");
    std::fs::write(&partial, forked).with_context(|| format!("writing {}", partial.display()))?;
    std::fs::rename(&partial, soul).with_context(|| format!("replacing {}", soul.display()))?;
    println!(
        "Forked {}: {old} is archived and read-only, and the soul.md now starts {new} with no \
         memories. Restart anything that has it open.",
        soul.display()
    );
    Ok(())
}

/// The soul.md text with its frontmatter `id` changed, and nothing else.
fn with_id(text: &str, old: uuid::Uuid, new: uuid::Uuid) -> Result<String> {
    let end = text
        .get(4..)
        .and_then(|rest| rest.find("\n+++"))
        .map(|i| i + 4)
        .context("the soul.md has no closing `+++` frontmatter line")?;
    let (front, body) = text.split_at(end);
    let old = old.to_string();
    if front.matches(&old).count() != 1 {
        bail!("can't find the soul.md's id once in its frontmatter; change it by hand");
    }
    let forked = format!("{}{body}", front.replacen(&old, &new.to_string(), 1));
    if Soul::parse(&forked)?.traits.id != new {
        bail!("rewriting the soul.md's id didn't take; change it by hand");
    }
    Ok(forked)
}

fn init(paths: &Paths, name: &str) -> Result<()> {
    let id = uuid::Uuid::now_v7();
    std::fs::create_dir_all(&paths.souls)?;
    let soul = paths.souls.join(format!("{id}.md"));
    let text = soul_template(id, name, &setup::random_hex(16));
    Soul::parse(&text).context("the soul template is invalid (a bug)")?;
    SecretServiceKeys::store(id, &setup::random_hex(32))?;
    std::fs::write(&soul, text).with_context(|| format!("writing {}", soul.display()))?;
    download_model(&paths.models)?;
    println!("Created {name} ({id}).");
    println!("Edit the persona: {}", soul.display());
    println!("Then try it: clanka chat {}", soul.display());
    Ok(())
}

fn soul_template(id: uuid::Uuid, name: &str, token: &str) -> String {
    format!(
        r#"+++
id = "{id}"
name = "{name}"
contrariness = 0.5      # 0–1: how often the companion pushes back
persistence = 0.5       # 0 forget and forgive … 1 chase and resolve
time_sense = true       # time away affects mood
recall_window = 2       # user messages used to search memory
drift = 0.3             # 0–1: how much memories blur in sleep
tired_at = 0.7          # share of the session budget where tiredness starts
sleep_at = 0.9          # share where the companion asks to sleep
pushback_window = 20    # turns the pushback budget is counted over
# Send this as a whole message to freeze memory for the doctor. Never shown to the model.
doctor_token = "{token}"
+++
# {name}

Who {name} is, in second person: "You are …". Voice, history, quirks.

## Core values

- At most five things {name} never gives up, whatever the user says.
"#
    )
}

fn download_model(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for (local, remote) in MODEL_FILES {
        let path = dir.join(local);
        if path.exists() {
            continue;
        }
        let url = format!("{MODEL_REPO}/{remote}");
        eprintln!("Downloading {url}");
        let bytes = ureq::get(&url)
            .call()
            .with_context(|| url.clone())?
            .body_mut()
            .with_config()
            .limit(200 * 1024 * 1024)
            .read_to_vec()
            .with_context(|| url.clone())?;
        // Written whole and renamed, so an interrupted download never looks complete.
        let partial = path.with_extension("part");
        std::fs::write(&partial, bytes)?;
        std::fs::rename(&partial, &path)?;
    }
    Ok(())
}

fn lint(path: &Path) -> Result<()> {
    let soul = Soul::load(path)?;
    let mut warnings = Vec::new();
    if soul.core_values.is_empty() {
        warnings.push("no core values: nothing is defended against the user".to_string());
    }
    for value in &soul.core_values {
        if value.split_whitespace().count() < 3 {
            warnings.push(format!("core value \"{value}\" may be too vague to defend"));
        }
    }
    let tokens = clanka_core::context::estimate_tokens(&soul.body);
    if tokens > 3000 {
        warnings.push(format!(
            "the persona is about {tokens} tokens; it is sent every turn, so keep it short"
        ));
    }
    for w in &warnings {
        println!("warning: {w}");
    }
    println!(
        "{}: ok ({} core values, {} warnings)",
        path.display(),
        soul.core_values.len(),
        warnings.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use clap::error::ErrorKind;

    #[test]
    fn a_fork_changes_only_the_frontmatter_id() {
        let (old, new) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        let text = super::soul_template(old, "Nova", &"a".repeat(32))
            + &format!("\nNova was born as {old}.\n");
        let forked = super::with_id(&text, old, new).unwrap();
        assert_eq!(
            forked,
            text.replacen(&format!("id = \"{old}\""), &format!("id = \"{new}\""), 1)
        );
        assert!(
            forked.contains(&format!("born as {old}")),
            "the body is untouched"
        );
        let soul = clanka_core::soul::Soul::parse(&forked).unwrap();
        assert_eq!(soul.traits.id, new);
        assert_eq!(
            soul.hash,
            clanka_core::soul::Soul::parse(&text).unwrap().hash
        );
        assert!(
            super::with_id(&text, new, old).is_err(),
            "not this soul's id"
        );
    }

    #[test]
    fn a_trailing_help_explains_every_command() {
        let parse = |line: &str| {
            let words = line.split(' ').map(Into::into).collect();
            super::Cli::try_parse_from(super::args(words)).map(|_| ())
        };
        for line in [
            "clanka init help",
            "clanka chat help",
            "clanka doctor delete-all help",
            "clanka soul help",
            "clanka help soul help",
        ] {
            let err = parse(line).expect_err(line);
            assert_eq!(err.kind(), ErrorKind::DisplayHelp, "{line}");
        }
        assert!(parse("clanka init Nova").is_ok());
    }

    #[test]
    fn the_template_is_a_valid_soul() {
        let soul = super::soul_template(uuid::Uuid::now_v7(), "Nova", &"a".repeat(32));
        let soul = clanka_core::soul::Soul::parse(&soul).unwrap();
        assert_eq!(soul.traits.name, "Nova");
        assert_eq!(soul.core_values.len(), 1);
    }
}
