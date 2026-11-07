//! `clanka`: set up companions, check their souls and serve them over MCP.

use anyhow::{Context, Result, bail};
use clanka::setup::{
    self, Config, MODEL_FILES, MODEL_REPO, Paths, SecretServiceKeys, SharedEmbedder,
};
use clanka_core::soul::Soul;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "clanka",
    version,
    about = "Long-term memory for chat companions"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new companion: a soul.md to edit, its database key and the embedding model.
    Init {
        /// The companion's display name.
        name: String,
    },
    /// Soul commands.
    Soul {
        #[command(subcommand)]
        command: SoulCommand,
    },
    /// Talk with a companion in the terminal, on its [model].
    Chat {
        /// The companion's soul.md.
        soul: PathBuf,
        /// Estimated tokens the memory context may use per turn.
        #[arg(long, default_value_t = 6000)]
        budget: usize,
    },
    /// Serve a companion to an MCP host over stdio, for trying it out.
    Mcp {
        /// The companion's soul.md.
        soul: PathBuf,
        /// Estimated tokens the memory context may use per turn.
        #[arg(long, default_value_t = 6000)]
        budget: usize,
    },
}

#[derive(Subcommand)]
enum SoulCommand {
    /// Check a soul.md for errors.
    Lint { soul: PathBuf },
    /// Print the persona without its frontmatter, for a host's own persona
    /// file such as Hermes's SOUL.md. The frontmatter holds the doctor token,
    /// which must never reach the model.
    Persona { soul: PathBuf },
}

fn main() -> Result<()> {
    let paths = Paths::get()?;
    match Cli::parse().command {
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
    if let clanka_core::PersonaStatus::Changed { .. } = chat.persona_status() {
        bail!(
            "{} changed since its memories were written; choose continue, update or fork first",
            soul.display()
        );
    }
    let writer = match &config.model {
        Some(m) => Some(setup::open(paths, soul, embedder, Some(m))?),
        None => None,
    };
    Ok((chat, writer))
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
    #[test]
    fn the_template_is_a_valid_soul() {
        let soul = super::soul_template(uuid::Uuid::now_v7(), "Nova", &"a".repeat(32));
        let soul = clanka_core::soul::Soul::parse(&soul).unwrap();
        assert_eq!(soul.traits.name, "Nova");
        assert_eq!(soul.core_values.len(), 1);
    }
}
