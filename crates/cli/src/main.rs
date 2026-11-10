//! `clanka`: set up companions, check their souls and serve them over MCP.

use anyhow::{Context, Result, bail};
use clanka::setup::{self, Config, Embedding, Keys, Paths, SharedEmbedder};
use clanka_core::PersonaStatus;
use clanka_core::soul::Soul;
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

mod doctor;
mod hermes;
mod probe;
mod transfer;

use doctor::DoctorCommand;

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
        ~/.config/clanka/config.toml     the [model] chat and memory writing run on, and\n\
        \x20                                  optional [recall], [embedding] and [keys] settings\n  \
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
    ///
    /// On a machine with no Secret Service, put `[keys]` and `store = "file"` in
    /// ~/.config/clanka/config.toml first: the key then goes in a file only you can read,
    /// ~/.config/clanka/keys/<id>.key.
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
        clanka hermes profile nova ~/.config/clanka/souls/<id>.md   # refresh its SOUL.md")]
    Soul {
        #[command(subcommand)]
        command: SoulCommand,
    },
    /// The doctor's tools for when a companion's memory goes wrong.
    ///
    /// Look first: `log` shows every write and `check-facts` the memories that claim things
    /// never said. To repair, freeze the companion: while it is frozen nothing writes to its
    /// memory, and `rollback`, `reinterpret` and `delete-all` refuse to run unless it is.
    /// Unfreeze when you are done. `delete` removes single memories and `reembed` moves the
    /// companion to another embedding model.
    #[command(after_long_help = "Example:\n  \
        clanka doctor log ~/.config/clanka/souls/<id>.md --since 6h\n  \
        clanka doctor freeze ~/.config/clanka/souls/<id>.md\n  \
        clanka doctor rollback ~/.config/clanka/souls/<id>.md --to 2026-03-09T14:30:05\n  \
        clanka doctor unfreeze ~/.config/clanka/souls/<id>.md")]
    Doctor {
        #[command(subcommand)]
        command: DoctorCommand,
    },
    /// Print what a companion remembers: mood, sections, open threads and every memory.
    ///
    /// Only reads the memory, so it is safe while a host has the companion open (a
    /// companion that was never opened gets its empty database first). For checking what a persona keeps from a conversation; `hermes/bench.sh` prints
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
    /// List the companions set up on this machine.
    ///
    /// One entry per soul.md under ~/.config/clanka/souls/: the name, the soul.md path every
    /// other command takes, the ID and how big its memory is. Opens no database.
    #[command(after_long_help = "Example:\n  clanka list")]
    List,
    /// Copy a companion into a new directory, to move it to another machine or keep a copy.
    ///
    /// Writes three files: soul.md, companion.db (a snapshot of the encrypted database,
    /// safe to take while the companion is in use) and key, the database key in plain
    /// text. Whoever has the directory can read every memory, so move it over a channel
    /// you trust and delete it afterwards. Changes nothing here (a companion that was never
    /// opened gets its empty database first).
    #[command(after_long_help = "Example:\n  \
        clanka export ~/.config/clanka/souls/<id>.md ~/nova-export\n  \
        scp -r ~/nova-export other-machine:   # then there: clanka import ~/nova-export")]
    Export {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        /// The directory to create; it must not exist yet.
        dir: PathBuf,
    },
    /// Set up the companion a `clanka export` directory holds.
    ///
    /// Stores its key (in the Secret Service, or a key file with `[keys] store = "file"` in
    /// config.toml), then copies its database and soul.md into place and opens it once to
    /// check the key fits. Never overwrites: it refuses if a companion with that ID is
    /// already here. If its vectors came from another embedding model than this machine's
    /// config names, it says to run `clanka doctor reembed`.
    #[command(after_long_help = "Example:\n  clanka import ~/nova-export")]
    Import {
        /// A directory `clanka export` wrote.
        dir: PathBuf,
    },
    /// Print what one query brings up from memory, with the numbers behind the order.
    ///
    /// The same search a turn runs, on the query alone: relevance (how close the memory is
    /// to the query), the final score after level, age, mood and open threads, and the
    /// memory. For seeing why something does or doesn't come up before changing `[recall]`
    /// in config.toml. Only reads the memory: nothing is marked as recalled. (A companion
    /// that was never opened gets its empty database first.)
    #[command(after_long_help = "Example:\n  \
        clanka recall ~/.config/clanka/souls/<id>.md \"how was the concert\"")]
    Recall {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        /// What the user would say.
        query: String,
    },
    /// Measure recall over a file of queries and what each should bring up.
    ///
    /// The cases file is TOML, one [[case]] per query: `query`, `expect` (texts that must
    /// appear in a recalled memory or in the value of one of its facts, in any letter case)
    /// and `reject` (texts that must not); a case needs at least one of the two. Prints
    /// each case, then how many expected memories came up and their mean
    /// reciprocal rank; exits non-zero on a miss or an unwanted memory. Change a `[recall]`
    /// value in config.toml, run it again and compare. Only reads the memory.
    #[command(after_long_help = "Example cases.toml:\n  \
        [[case]]\n  \
        query = \"how was the concert\"\n  \
        expect = [\"Laura\", \"concert\"]\n  \
        reject = [\"dentist\"]\n\n\
        Then:\n  clanka eval ~/.config/clanka/souls/<id>.md cases.toml\n\n\
        [recall] in config.toml takes limit, seared_limit, min_relevance, min_score, \
        fts_weight, mood_weight, thread_boost, faint_half_life, normal_half_life, vivid_floor \
        and level_weights; `clanka recall` prints the limits in use.")]
    Eval {
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
        /// The TOML file of cases.
        cases: PathBuf,
    },
    /// Download the embedding model config.toml names, if it is not here yet.
    ///
    /// The model turns text into the vectors recall searches. The default is a small
    /// English one (about 23 MB); `[embedding] model = "multilingual"` in config.toml picks
    /// a multilingual one (about 250 MB) for companions that talk in other languages. A
    /// companion keeps the model its vectors were built with: after changing the setting,
    /// run this, then `clanka doctor reembed` on each companion.
    ///
    /// `[embedding] duplicate_similarity` (above 0, at most 1; 0.92 unless set) is how alike
    /// a new memory and one already kept must be for the new one to be refused as a copy. Models spread similar texts differently, so it may need changing with the
    /// model.
    #[command(after_long_help = "Example:\n  clanka embedding")]
    Embedding,
    /// Run companions on Hermes: a profile per companion, and Nightshade, the agent that
    /// interviews you and makes one.
    ///
    /// Needs the clanka plugin that hermes/install.sh builds and installs under
    /// ~/.hermes/plugins/clanka, and `hermes` on PATH. Both commands drive the Hermes CLI
    /// and are safe to run again: they set the same values and rewrite the profile's
    /// SOUL.md. They work the same from a shell and from an agent's terminal inside Hermes.
    #[command(after_long_help = "Example:\n  \
        clanka hermes profile nova ~/.config/clanka/souls/<id>.md\n  \
        hermes -p nova chat\n  \
        clanka hermes nightshade --model claude-opus-5-5\n  \
        hermes -p nightshade chat")]
    Hermes {
        #[command(subcommand)]
        command: HermesCommand,
    },
}

const SOUL_HELP: &str = "The companion's soul.md, as printed by `clanka init`";

#[derive(Subcommand)]
enum HermesCommand {
    /// Make a Hermes profile that is one companion and nothing else.
    ///
    /// clanka's two tools are the only toolset on every platform, listed directly rather
    /// than behind Hermes's tool-search bridge; Hermes's own memory, user profile, coding
    /// brief, toolchain probe and agent-style guidance are off; the terminal and Telegram
    /// notes are a line each; and SOUL.md is the persona. Each turn then carries the
    /// persona and the conversation instead of Hermes's whole toolbox: on Haiku the system
    /// prompt and tools went from about 19k tokens on a stock profile to about 1.2k. The
    /// root's own `providers:`, model aliases and reasoning overrides are copied in, and
    /// the keys those providers read are copied from ~/.hermes/.env into the profile's,
    /// since a profile loads only its own .env. Run it again after editing the persona to
    /// refresh SOUL.md; a toolset you added next to clanka on a platform (`hermes -p <profile>
    /// config set platform_toolsets.telegram '[clanka, image_gen]'`) is kept. A gateway
    /// already serving the profile re-reads its config on its own.
    #[command(after_long_help = "Example:\n  \
        clanka hermes profile nova ~/.config/clanka/souls/<id>.md\n  \
        hermes -p nova chat")]
    Profile {
        /// The Hermes profile to make or refresh (lower case, one word).
        profile: String,
        #[arg(help = SOUL_HELP)]
        soul: PathBuf,
    },
    /// Make the Nightshade profile: an agent that interviews you about the companion you
    /// want, writes its soul.md, lints it and wires it in with `clanka hermes profile`.
    ///
    /// It is the soul-writer skill as a person to talk to. The terminal and file tools are
    /// all it has, and hermes/nightshade/SOUL.md is the whole of what it knows, with the
    /// path of this binary filled in. The root's own `providers:`, model aliases and their
    /// keys are shared with it, so `/model <alias>` works in its chat. Without --model it
    /// runs on the Hermes default; a stronger model interviews better and the session is
    /// short.
    #[command(after_long_help = "Example:\n  \
        clanka hermes nightshade --model claude-opus-5-5\n  \
        hermes -p nightshade chat")]
    Nightshade {
        /// The Hermes profile to make or refresh.
        #[arg(long, default_value = "nightshade")]
        profile: String,
        /// The model it runs on, as Hermes names it; the Hermes default when unset.
        #[arg(long)]
        model: Option<String>,
    },
}

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
        Command::Doctor { command } => doctor::run(&paths, command),
        Command::List => transfer::list(&paths),
        Command::Export { soul, dir } => transfer::export(&paths, &soul, &dir),
        Command::Import { dir } => transfer::import(&paths, &dir),
        Command::Recall { soul, query } => probe::recall(&paths, &soul, &query),
        Command::Eval { soul, cases } => probe::eval(&paths, &soul, &cases),
        Command::Embedding => {
            let embedding = Config::load(&paths)?.embedding.model;
            download_model(&paths, embedding)?;
            println!(
                "The embedding model is {} in {}.",
                embedding.id(),
                embedding.dir(&paths).display()
            );
            Ok(())
        }
        Command::Memories { soul } => memories(&paths, &soul),
        Command::Hermes {
            command: HermesCommand::Profile { profile, soul },
        } => hermes::profile(&profile, &soul),
        Command::Hermes {
            command: HermesCommand::Nightshade { profile, model },
        } => hermes::nightshade(&profile, model.as_deref()),
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
            "#{} [{urgency}, {}, {}] {}",
            m.id,
            m.level.as_str(),
            clanka_core::context::date(m.created_at),
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
            "#{} [{}, {}{valence}, {}] {}{facts}",
            m.id,
            m.kind.as_str(),
            m.level.as_str(),
            clanka_core::context::date(m.created_at),
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
    Keys::new(paths, &Config::load(paths)?).store(new, &setup::random_hex(32))?;
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
    let config = Config::load(paths)?;
    Keys::new(paths, &config).store(id, &setup::random_hex(32))?;
    std::fs::write(&soul, text).with_context(|| format!("writing {}", soul.display()))?;
    download_model(paths, config.embedding.model)?;
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

/// Downloads the embedding model's files that are not there yet.
fn download_model(paths: &Paths, embedding: Embedding) -> Result<()> {
    let dir = embedding.dir(paths);
    std::fs::create_dir_all(&dir)?;
    for (local, remote) in embedding.files() {
        let path = dir.join(local);
        if path.exists() {
            continue;
        }
        let url = format!("{}/{remote}", embedding.repo());
        eprintln!("Downloading {url}");
        let mut response = ureq::get(&url).call().with_context(|| url.clone())?;
        let mut body = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024 * 1024)
            .reader();
        // Written whole and renamed, so an interrupted download never looks complete.
        let partial = path.with_extension("part");
        let mut file = std::fs::File::create(&partial)?;
        std::io::copy(&mut body, &mut file).with_context(|| url.clone())?;
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

/// Lines of the body that quote the companion's voice (a quoted span of four
/// words or more), and how many of those quotes end on a question. A small
/// model says its examples back word for word, so they decide how it ends.
fn voice_lines(body: &str) -> (usize, usize) {
    let mut quoted = 0;
    let mut questions = 0;
    for line in body.lines() {
        let Some(start) = line.find('"') else {
            continue;
        };
        let Some(len) = line[start + 1..].rfind('"') else {
            continue;
        };
        let quote = &line[start + 1..start + 1 + len];
        if quote.split_whitespace().count() < 4 {
            continue;
        }
        quoted += 1;
        if quote.trim_end().ends_with('?') {
            questions += 1;
        }
    }
    (quoted, questions)
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
        let (quoted, questions) = voice_lines(&soul.body);
        if quoted >= 2 && questions * 2 >= quoted {
            warnings.push(format!(
                "{questions} of {quoted} lines in the voice end on a question; a small model \
                 recites its examples, so an evening of these is an interview. End most of them \
                 on a statement"
            ));
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
            "clanka doctor log help",
            "clanka doctor rollback help",
            "clanka list help",
            "clanka export help",
            "clanka import help",
            "clanka recall help",
            "clanka eval help",
            "clanka embedding help",
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
        assert!(parse("clanka doctor log x.md --since 6h").is_ok());
        assert!(parse("clanka doctor rollback x.md --to 2026-03-09T14:30:05").is_ok());
        assert!(parse("clanka doctor delete x.md 12 15").is_ok());
        assert!(parse("clanka export x.md out").is_ok());
        assert!(parse("clanka recall x.md concert").is_ok());
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
