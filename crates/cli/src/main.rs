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
    #[command(after_long_help = "Example:\n  clanka init Nova\n  \
        clanka init Nova --from body.md     # a persona you or a model already wrote")]
    Init {
        /// The companion's display name.
        name: String,
        /// A Markdown persona body to use instead of the template; `-` reads stdin.
        /// The body should start at `# Name`; the frontmatter is added. Lint it after.
        #[arg(long, value_name = "FILE")]
        from: Option<PathBuf>,
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
    /// Print what a companion remembers: mood, sections, open threads and every memory.
    ///
    /// Reads the memory and changes nothing, so it is safe while a host has the companion
    /// open. For checking what a persona keeps from a conversation; `hermes/bench.sh` prints
    /// it after a scripted evening.
    #[command(after_long_help = "Example:\n  clanka memories ~/.config/clanka/souls/<id>.md")]
    Memories {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
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
    /// Check a soul.md for errors and say what the persona costs.
    ///
    /// Fails if the frontmatter or the persona does not parse. Warns, without failing, when
    /// a TODO from the template is still there, when there are no core values, when a core
    /// value is too vague to defend, or when the persona is long enough to crowd every turn.
    /// Prints the estimated tokens the persona adds to each chat turn and each memory write.
    /// Changes nothing.
    #[command(
        after_long_help = "Example:\n  clanka soul lint ~/.config/clanka/souls/<id>.md\n  \
        clanka soul lint --json --strict body.md   # in a script or an agent loop"
    )]
    Lint {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        /// Print the report as one JSON object instead of text.
        #[arg(long)]
        json: bool,
        /// Exit non-zero while there are warnings, not only on errors.
        #[arg(long)]
        strict: bool,
    },
    /// Print the guide to writing a soul, for you or for a model that will write one.
    ///
    /// The same text as the `clanka-soul-writer` skill under skills/ in the repository: what
    /// a good body looks like, what each trait does, a checklist and a short example. Pipe it
    /// to any model as its instructions, then `clanka init <Name> --from` what it wrote.
    #[command(after_long_help = "Example:\n  clanka soul guide | less")]
    Guide,
    /// Have a model write a soul from a short brief, lint it and create the companion.
    ///
    /// Sends the guide and your brief to the [model] in ~/.config/clanka/config.toml, or to
    /// the --writer command, takes the body it writes, runs the lint, and sends the warnings
    /// back for another try up to --rounds times. Then creates the companion as `clanka init
    /// --from` would and prints the lint report. The persona is a draft: read it before
    /// anyone talks to it.
    #[command(after_long_help = "Examples:\n  \
        clanka soul draft Mara --brief \"a night-shift radiographer in a coastal town, dry, kind\"\n  \
        clanka soul draft Mara --brief \"$(cat brief.md)\" \\\n      \
        --writer 'hermes chat -Q -m claude-opus-5-5 --query-file -'\n\n\
        `clanka soul guide` says what a brief should hold (section \"The brief\").")]
    Draft {
        /// The companion's display name.
        name: String,
        /// Who the companion is, in a sentence or a paragraph; `clanka soul guide` says what to put in.
        #[arg(long)]
        brief: String,
        /// Most times the warnings go back to the model for a rewrite.
        #[arg(long, default_value_t = 2)]
        rounds: usize,
        /// A shell command that writes the body instead of the [model]: it reads the whole
        /// prompt on stdin and prints the body. Any model with a command line will do.
        #[arg(long, value_name = "COMMAND")]
        writer: Option<String>,
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
        Command::Init { name, from } => {
            let body = match from {
                None => template_body(&name),
                Some(path) if path == Path::new("-") => std::io::read_to_string(std::io::stdin())?,
                Some(path) => std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?,
            };
            let soul = create(&paths, &name, &body)?;
            println!("Edit the persona: {}", soul.display());
            println!("Then try it: clanka chat {}", soul.display());
            Ok(())
        }
        Command::Soul {
            command: SoulCommand::Lint { soul, json, strict },
        } => {
            let report = LintReport::of(&Soul::load(&soul)?);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report.text(&soul));
            }
            if strict && !report.warnings.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Soul {
            command: SoulCommand::Guide,
        } => {
            println!("{}", guide());
            Ok(())
        }
        Command::Soul {
            command:
                SoulCommand::Draft {
                    name,
                    brief,
                    rounds,
                    writer,
                },
        } => draft(&paths, &name, &brief, rounds, writer.as_deref()),
        Command::Soul {
            command: SoulCommand::Persona { soul },
        } => {
            println!("{}", Soul::load(&soul)?.persona());
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
        Command::Memories { soul } => memories(&paths, &soul),
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

fn memories(paths: &Paths, soul: &Path) -> Result<()> {
    use clanka_core::model::{MemoryType, Section};
    let companion = setup::open(paths, soul, SharedEmbedder::load(paths)?, None)?;
    match companion.mood()? {
        Some(m) => println!(
            "Mood: valence {:+.1}, energy {:+.1}, warmth {:+.1}. Why: {}",
            m.valence, m.energy, m.warmth, m.cause
        ),
        None => println!("Mood: not set yet."),
    }
    if let Some((_, note)) = companion.last_turn_note()? {
        println!("Note for the next reply: {note}");
    }
    for section in [Section::AboutUser, Section::Relationship] {
        println!(
            "\n{}:\n{}",
            section.as_str(),
            companion
                .section(section)?
                .filter(|c| !c.trim().is_empty())
                .as_deref()
                .unwrap_or("(empty)")
        );
    }
    let all = companion.active_memories()?;
    let (threads, memories): (Vec<_>, Vec<_>) =
        all.iter().partition(|m| m.kind == MemoryType::OpenThread);
    println!("\nOpen threads ({}):", threads.len());
    for m in threads {
        let urgency = m.urgency.map(|u| u.as_str()).unwrap_or("?");
        println!(
            "#{} [{urgency}, {}] {}",
            m.id,
            m.level.as_str(),
            m.interpretation
        );
    }
    println!("\nMemories ({}):", memories.len());
    for m in memories {
        let valence = m.valence.map(|v| format!(", {v:+.1}")).unwrap_or_default();
        let facts = if m.facts.is_empty() {
            String::new()
        } else {
            format!(" facts {}", serde_json::Value::Object(m.facts.clone()))
        };
        println!(
            "#{} [{}, {}{valence}] {}{facts}",
            m.id,
            m.kind.as_str(),
            m.level.as_str(),
            m.interpretation
        );
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

/// Creates a companion from a persona body: a soul.md with fresh frontmatter,
/// its database key and the embedding model. Returns the soul.md path.
fn create(paths: &Paths, name: &str, body: &str) -> Result<PathBuf> {
    let id = uuid::Uuid::now_v7();
    let text = soul_text(id, name, &setup::random_hex(16), body);
    Soul::parse(&text).context("the persona body does not make a valid soul.md")?;
    std::fs::create_dir_all(&paths.souls)?;
    let soul = paths.souls.join(format!("{id}.md"));
    SecretServiceKeys::store(id, &setup::random_hex(32))?;
    std::fs::write(&soul, text).with_context(|| format!("writing {}", soul.display()))?;
    download_model(&paths.models)?;
    println!("Created {name} ({id}).");
    Ok(soul)
}

/// The soul-writing guide: the `clanka-soul-writer` skill without its frontmatter.
fn guide() -> &'static str {
    const SKILL: &str = include_str!("../../../skills/soul-writer/SKILL.md");
    SKILL
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(SKILL, |(_, body)| body)
        .trim()
}

/// The configured model, or the `--writer` command, writes a body from the
/// brief, hears the lint warnings back up to `rounds` times, and the result
/// becomes a companion.
fn draft(
    paths: &Paths,
    name: &str,
    brief: &str,
    rounds: usize,
    writer: Option<&str>,
) -> Result<()> {
    use clanka_core::deps::{Llm, LlmMessage, LlmRequest};
    let llm = match writer {
        Some(_) => None,
        None => match Config::load(paths)?.model {
            Some(model) => Some(clanka::llm::OpenAiModel::new(model)),
            None => bail!(
                "no model configured to write the soul; add a [model] section to {}, pass \
                 --writer with a command that runs one, or write the body with any model \
                 from `clanka soul guide` and pass it to `clanka init --from`",
                paths.config_file().display()
            ),
        },
    };
    let write = |r: &LlmRequest| -> Result<String> {
        match (writer, &llm) {
            (Some(command), _) => write_through(command, r),
            (None, Some(llm)) => Ok(llm.complete(r)?.text),
            (None, None) => unreachable!("a writer or a model was chosen above"),
        }
    };
    let mut request = LlmRequest {
        system: format!(
            "{}\n\nYou are writing the soul.md body for a new companion named {name}. Reply with \
             the Markdown body only: it starts with the line `# {name}`, has no frontmatter and \
             no code fence, and follows the guide above.",
            guide()
        ),
        messages: vec![LlmMessage::user(format!("The brief: {brief}"))],
        tools: Vec::new(),
        temperature: 0.8,
    };
    let mut body = String::new();
    let mut report = None;
    for round in 0..=rounds {
        body = unfenced(&write(&request)?);
        let r = lint_draft(name, &body);
        eprintln!(
            "round {}: {} words, {} warnings",
            round + 1,
            body.split_whitespace().count(),
            r.warnings.len()
        );
        if r.warnings.is_empty() || round == rounds {
            report = Some(r);
            break;
        }
        request.messages.push(LlmMessage::assistant(body.clone()));
        request.messages.push(LlmMessage::user(format!(
            "`clanka soul lint` says:\n- {}\n\nReply with the whole corrected body.",
            r.warnings.join("\n- ")
        )));
    }
    let soul = create(paths, name, &body)?;
    print!(
        "{}",
        report.expect("the loop ran at least once").text(&soul)
    );
    println!("Read it before anyone talks to it: {}", soul.display());
    Ok(())
}

/// Lints a drafted body as the soul it would become. The placeholder id must
/// pass the same checks as a real one, or every round reports a warning the
/// writer can't fix.
fn lint_draft(name: &str, body: &str) -> LintReport {
    let text = soul_text(uuid::Uuid::now_v7(), name, &"0".repeat(32), body);
    match Soul::parse(&text) {
        Ok(soul) => LintReport::of(&soul),
        Err(e) => LintReport::invalid(e.to_string()),
    }
}

/// Runs a `--writer` command with the whole request as one prompt on stdin: the
/// system prompt, then each message, a later round's draft marked as such. What
/// it prints is the reply.
fn write_through(command: &str, request: &clanka_core::deps::LlmRequest) -> Result<String> {
    use clanka_core::deps::Role;
    use std::io::Write;
    use std::process::{Command as Process, Stdio};
    let mut prompt = request.system.clone();
    for m in &request.messages {
        prompt.push_str("\n\n");
        if m.role == Role::Assistant {
            prompt.push_str("Your previous draft:\n\n");
        }
        prompt.push_str(&m.content);
    }
    let mut child = Process::new("sh")
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting the writer command {command:?}"))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(prompt.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("the writer command {command:?} failed ({})", out.status);
    }
    let text = String::from_utf8(out.stdout).context("the writer command printed invalid UTF-8")?;
    if text.trim().is_empty() {
        bail!("the writer command {command:?} printed nothing");
    }
    Ok(text)
}

/// A model's reply without a Markdown code fence around it, if it added one.
fn unfenced(text: &str) -> String {
    let text = text.trim();
    match text.strip_prefix("```") {
        Some(rest) => {
            let rest = rest.split_once('\n').map_or("", |(_, r)| r);
            rest.strip_suffix("```").unwrap_or(rest).trim().to_owned()
        }
        None => text.to_owned(),
    }
}

/// A whole soul.md: frontmatter for `id`, `name` and `token` over `body`.
fn soul_text(id: uuid::Uuid, name: &str, token: &str, body: &str) -> String {
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
{}
"#,
        body.trim_end()
    )
}

#[cfg(test)]
fn soul_template(id: uuid::Uuid, name: &str, token: &str) -> String {
    soul_text(id, name, token, &template_body(name))
}

/// The commented template a new soul.md starts from.
fn template_body(name: &str) -> String {
    format!(
        r#"# {name}

<!-- Everything below that is not in a comment like this one is {name}: the chat
model reads it every turn, and {name} reads it again when deciding what to
remember, so it shapes what gets kept as much as how {name} talks. Comments are
dropped before any of that, so leave yourself as many notes as you like.
Replace every TODO; `clanka soul lint` points out the ones left. Aim for about
400 words: a small model follows a short, concrete persona better than a long
one. -->

<!-- Who {name} is, in second person. Age, look, situation, and one or two
things that have nothing to do with the user. Three or four sentences. -->
You are {name}. TODO

## How you talk

<!-- Rules a model can check, not adjectives: how long a reply is, what you
never do, and a line or two in your actual voice. A small model copies examples
more than it follows descriptions. These bullets are a start; make them yours. -->
- Short, spoken replies: one or two sentences. More only when the user brought something real.
- No lists, no headings, no "as an AI", no apologising for having an opinion.
- You don't repeat a bit. A tease or a pet name once in a conversation, not in every reply.
- Good news: TODO how you react, in your own words. "…"
- Disagreement: ask rather than lecture. "Wait, do you actually believe that?" Then let it go.
- A hard day: TODO what you do first. Quiet is allowed.
- Questions: one at a time, and only the one you actually want answered.

## What you care about

<!-- What {name} notices, asks about and lights up at, and what bores {name}.
This is what {name} remembers from a conversation, so be specific. -->
TODO

## Core values

<!-- Three to five things {name} does not give up, whatever the user says. Each
should be concrete enough that you could tell when it was broken. They are
defended against the user, so keep them few; everything else is personality. -->
- You tell the truth, even when it stings.
- You never pretend to be someone you're not.
- TODO
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

/// Lines of a persona still holding a `TODO` from the template. The persona is
/// read without its comments, so a note about TODOs is not one.
fn todos(body: &str) -> Vec<&str> {
    body.lines()
        .map(str::trim)
        .filter(|line| line.contains("TODO"))
        .collect()
}

/// Above this the persona, sent every turn and again for every memory write,
/// starts to cost more than it adds.
const LONG_PERSONA_TOKENS: usize = 1000;

/// What `clanka soul lint` has to say about a soul: the same report as text
/// for a person and as JSON for a script or a model rewriting the persona.
#[derive(Debug, serde::Serialize)]
struct LintReport {
    /// False when the text does not even parse as a soul.md; then `warnings`
    /// holds the parse error and the counts are zero.
    valid: bool,
    core_values: usize,
    /// Estimated tokens the persona adds to every chat turn.
    chat_tokens: usize,
    /// Estimated tokens of the memory writer's system prompt: persona,
    /// instructions and actions.
    memory_tokens: usize,
    warnings: Vec<String>,
}

impl LintReport {
    fn of(soul: &Soul) -> Self {
        let mut warnings = Vec::new();
        for line in todos(&soul.body) {
            warnings.push(format!("still to fill in: \"{line}\""));
        }
        if soul.core_values.is_empty() {
            warnings.push("no core values: nothing is defended against the user".to_string());
        }
        for value in &soul.core_values {
            if value.split_whitespace().count() < 3 {
                warnings.push(format!("core value \"{value}\" may be too vague to defend"));
            }
        }
        let chat_tokens = clanka_core::context::estimate_tokens(&soul.persona());
        let memory_tokens = clanka_core::context::estimate_tokens(
            &clanka_core::writer::reflection_system_prompt(soul),
        );
        if chat_tokens > LONG_PERSONA_TOKENS {
            warnings.push(format!(
                "the persona is about {chat_tokens} tokens; it is sent every turn and again for \
                 every memory write, so keep it short"
            ));
        }
        Self {
            valid: true,
            core_values: soul.core_values.len(),
            chat_tokens,
            memory_tokens,
            warnings,
        }
    }

    fn invalid(error: String) -> Self {
        Self {
            valid: false,
            core_values: 0,
            chat_tokens: 0,
            memory_tokens: 0,
            warnings: vec![format!("not a valid soul.md: {error}")],
        }
    }

    fn text(&self, path: &Path) -> String {
        let mut out = String::new();
        for w in &self.warnings {
            out += &format!("warning: {w}\n");
        }
        out += &format!(
            "{}: {} ({} core values, {} warnings)\n",
            path.display(),
            if self.valid { "ok" } else { "invalid" },
            self.core_values,
            self.warnings.len()
        );
        out += &format!(
            "The persona is about {} tokens on every chat turn; writing memory after the turn \
             sends about {} (persona, instructions and actions). Estimates, before the \
             conversation and the memories recalled for it.\n",
            self.chat_tokens, self.memory_tokens
        );
        out
    }
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
        assert!(parse("clanka init Nova --from -").is_ok());
        assert!(parse("clanka soul lint --json --strict x.md").is_ok());
        assert!(parse("clanka soul draft Mara --brief dry").is_ok());
    }

    #[test]
    fn a_body_from_a_file_or_a_model_becomes_a_soul() {
        let body = "```markdown\n# Mara\n\nYou are Mara.\n\n## Core values\n\n- Say what you \
                    see, plainly.\n```";
        let body = super::unfenced(body);
        assert!(body.starts_with("# Mara") && body.ends_with("plainly."));
        let text = super::soul_text(uuid::Uuid::now_v7(), "Mara", &"a".repeat(32), &body);
        let soul = clanka_core::soul::Soul::parse(&text).unwrap();
        assert_eq!(soul.traits.name, "Mara");
        assert_eq!(soul.core_values.len(), 1);
        let report = super::LintReport::of(&soul);
        assert!(report.valid && report.warnings.is_empty(), "{report:?}");
        assert!(report.chat_tokens > 0 && report.memory_tokens > report.chat_tokens);
        assert_eq!(super::unfenced("plain"), "plain", "no fence, no change");
        // The draft's own lint sees the same body as the soul it becomes.
        let draft = super::lint_draft("Mara", &body);
        assert!(draft.valid && draft.warnings.is_empty(), "{draft:?}");
    }

    #[test]
    fn the_guide_is_the_skill_without_its_frontmatter() {
        let guide = super::guide();
        assert!(guide.starts_with("# Writing a clanka soul"), "{guide:.40}");
        assert!(!guide.contains("\nname: clanka-soul-writer"));
        assert!(guide.contains("clanka soul lint"));
    }

    #[test]
    fn the_template_is_a_valid_soul_whose_notes_never_reach_the_model() {
        let text = super::soul_template(uuid::Uuid::now_v7(), "Nova", &"a".repeat(32));
        let soul = clanka_core::soul::Soul::parse(&text).unwrap();
        assert_eq!(soul.traits.name, "Nova");
        assert_eq!(soul.core_values.len(), 3);
        assert!(text.contains("<!--"), "the template guides its author");
        assert!(
            !soul.body.contains("<!--"),
            "and the guidance costs no tokens"
        );
        assert!(
            !soul.body.contains("lint"),
            "the notes are the only place lint is mentioned"
        );
        assert_eq!(
            super::todos(&soul.body).len(),
            5,
            "every blank an author must fill is a TODO, so lint can point at it"
        );
    }
}
