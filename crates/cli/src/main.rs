//! `clanka`: set up companions, check their souls and serve them over MCP.

use anyhow::{Context, Result, bail};
use clanka::setup::{self, Config, Embedding, Paths, SharedEmbedder};
use clanka_core::PersonaStatus;
use clanka_core::soul::Soul;
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

mod doctor;
mod hermes;
mod probe;
mod soul;
mod transfer;

use doctor::DoctorCommand;
use soul::{
    LintReport, create, draft, fork, guide, persona_continue, persona_update, template_body,
};

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
    /// Find the prose tics in replies: the shapes a model repeats.
    ///
    /// Reads each file as one reply (standard input without files) and prints the tics in
    /// it: a reveal after a denial, calling something the real or only one, a run of clipped
    /// fragments. They are guesses from a few words that miss four in ten of the reveals and
    /// half of the labels a reader would mark, and nearly every run of fragments, so one hit
    /// means little and the count over an evening is what to compare;
    /// `hermes/bench.sh` prints it.
    #[command(after_long_help = "Example:\n  clanka tics bench/nova/t*.txt\n  \
        pbpaste | clanka tics --json")]
    Tics {
        /// Files holding one reply each.
        replies: Vec<PathBuf>,
        /// Print the tics as one JSON array, an object per reply.
        #[arg(long)]
        json: bool,
    },
    /// Talk with a companion in the terminal, on its [model].
    ///
    /// Needs a [model] section in ~/.config/clanka/config.toml (base_url, model and
    /// api_key_env, which names the environment variable holding the key). Type a message
    /// and press Enter; each reply is remembered in the background. Send /sleep to end the
    /// conversation and turn it into memories, or send the soul's doctor_token to freeze
    /// the memory and quit without recording anything. Refuses to start if the persona
    /// changed since its memories were written.
    ///
    /// Markers at the start of a message: `>>N` asks for a reply of about N paragraphs.
    /// `~~`, as the first message after sleep, starts a dream: what you write is what
    /// happens to the companion, as "you", and it lives it without knowing it dreams. In a
    /// dream, `<<` shows it an echo of the past. It wakes when it chooses to, when the dream
    /// fills the session, or on /sleep; only fragments and its mood stay, and at most one
    /// thing an echo showed that you had also written while it was awake. Leaving mid-dream
    /// keeps nothing of it. Anywhere in a waking message, text between || and || is said out
    /// of the companion's earshot: it plays along without hearing it, and never keeps it.
    #[command(
        after_long_help = "Example:\n  clanka chat ~/.config/clanka/souls/<id>.md\n\n\
        A dream:\n  \
        you> ~~ You're standing in your mother's kitchen. It is too quiet.\n  \
        you> << >>3 Years ago, in this room, the judge reads the sentence aloud."
    )]
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
    /// opened gets its empty database first). Needs no embedding model, so it also works
    /// after `[embedding] model` changed and before `clanka doctor reembed`.
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
    /// Files already here are checked against the expected checksums, and any that is
    /// missing or damaged (an interrupted download) is fetched again.
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
    /// root's own `providers:`, model aliases, reasoning overrides and `model_overrides`
    /// are copied in, as is `auxiliary.title_generation.model_upgrade_enabled` when the
    /// root has it off, and the keys those providers read are copied from ~/.hermes/.env
    /// into the profile's, since a profile loads only its own .env. Run it again after
    /// editing the persona to refresh SOUL.md; a toolset you added next to clanka on a
    /// platform (`hermes -p <profile>
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
    /// value is too vague to defend, when the persona repeats a prose tic the companion would
    /// copy (a reveal after a denial, a maxim to end a voice line, clipped fragments) or asks
    /// for one ("you speak in images"), or when the persona is long enough to crowd every
    /// turn. A tic seen once is a note, which needs no change: the shapes are guessed from a
    /// few words, and one such sentence is how people talk.
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
        Command::Tics { replies, json } => tics(&replies, json),
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

fn tics(replies: &[PathBuf], json: bool) -> Result<()> {
    let mut texts = Vec::new();
    if replies.is_empty() {
        texts.push(("-".to_owned(), std::io::read_to_string(std::io::stdin())?));
    }
    for path in replies {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        texts.push((path.display().to_string(), text));
    }
    let found: Vec<_> = texts
        .iter()
        .map(|(name, text)| (name, clanka_core::tics::in_prose(text)))
        .collect();
    if json {
        let all: Vec<_> = found
            .iter()
            .map(|(name, tics)| {
                let tics: Vec<_> = tics
                    .iter()
                    .map(|t| serde_json::json!({"kind": t.kind.name(), "quote": t.quote}))
                    .collect();
                serde_json::json!({"reply": name, "tics": tics})
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    for (name, tics) in &found {
        for t in tics {
            println!("{name}: {}: \"{}\"", t.kind.name(), t.quote);
        }
    }
    let total: usize = found.iter().map(|(_, t)| t.len()).sum();
    let with = found.iter().filter(|(_, t)| !t.is_empty()).count();
    println!("{total} tics in {with} of {} replies", found.len());
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
    let (threads, memories): (Vec<_>, Vec<_>) = all
        .iter()
        .partition(|m| m.kind == MemoryType::OpenThread && m.resolved_at.is_none());
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
        let closed = if m.resolved_at.is_some() {
            ", closed"
        } else {
            ""
        };
        let dreamt = if m.dreamt { ", dreamt" } else { "" };
        println!(
            "#{} [{}{closed}{dreamt}, {}{valence}, {}] {}{facts}",
            m.id,
            m.kind.as_str(),
            m.level.as_str(),
            clanka_core::context::date(m.created_at),
            m.interpretation
        );
    }
    Ok(())
}

/// Downloads the embedding model's files that are missing or aren't the
/// expected ones (a download cut off by hand, a damaged disk).
fn download_model(paths: &Paths, embedding: Embedding) -> Result<()> {
    let dir = embedding.dir(paths);
    std::fs::create_dir_all(&dir)?;
    for (local, remote, sha256) in embedding.files() {
        let path = dir.join(local);
        if path.exists() && setup::sha256_file(&path)? == sha256 {
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
        // Written whole, checked and renamed, so a download that was
        // interrupted or answered with something else never looks complete.
        let partial = path.with_extension("part");
        let mut file = std::fs::File::create(&partial)?;
        std::io::copy(&mut body, &mut file).with_context(|| url.clone())?;
        drop(file);
        let found = setup::sha256_file(&partial)?;
        if found != sha256 {
            std::fs::remove_file(&partial)?;
            bail!("{url} is not the expected file: its SHA-256 is {found}, not {sha256}");
        }
        std::fs::rename(&partial, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use clap::error::ErrorKind;

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
}
