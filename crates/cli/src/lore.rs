//! `clanka lore`: writing the world's lore and keeping what companions add to
//! it in check (see [`clanka_core::lore`]).

use anyhow::{Context, Result, bail};
use clanka::setup::{self, Config, Paths, SharedEmbedder};
use clanka_core::Lore;
use clanka_core::lore::{Kind, Line, Provenance, parse_entry, valid_id};
use clanka_core::soul::Soul;
use clap::Subcommand;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum LoreCommand {
    /// Start an entry: a file with its frontmatter, to fill in with `edit`.
    ///
    /// The ID is the entry's path under the lore directory without `.md`: segments of a-z,
    /// 0-9 and -, separated by /, such as `moonrot` or `ashford/manor`. Makes the directory
    /// a git repository when git is installed and it isn't one yet, so every change to the
    /// lore is on record.
    #[command(
        after_long_help = "Example:\n  clanka lore new ashford/manor --title \"Ashford Manor\""
    )]
    New {
        id: String,
        /// The entry's title; the ID's last segment unless given.
        #[arg(long)]
        title: Option<String>,
    },
    /// Edit an entry in $EDITOR; it is written back only when its frontmatter parses.
    #[command(after_long_help = "Example:\n  clanka lore edit moonrot")]
    Edit { id: String },
    /// Replace an entry's whole file with another, or standard input with `-`.
    #[command(
        after_long_help = "Example:\n  clanka lore set moonrot --file moonrot.md\n  \
        pbpaste | clanka lore set moonrot --file -"
    )]
    Set {
        id: String,
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
    },
    /// List every entry: its title, and how many lines companions added.
    #[command(after_long_help = "Example:\n  clanka lore list")]
    List,
    /// Print an entry and every line standing in its ledger, with their IDs.
    #[command(after_long_help = "Example:\n  clanka lore show moonrot")]
    Show { id: String },
    /// Check every entry, every ledger and every soul's grants.
    ///
    /// An entry that doesn't parse, a file whose name is no entry ID and an unreadable
    /// ledger line are errors; a grant or an `opens` that names no entry is a warning.
    /// Exits non-zero on an error.
    #[command(after_long_help = "Example:\n  clanka lore lint")]
    Lint,
    /// List a companion's notes on the lore, private and released, newest first.
    #[command(
        after_long_help = "Example:\n  clanka lore notes ~/.config/clanka/souls/<id>.md moonrot"
    )]
    Notes {
        #[arg(help = crate::SOUL_HELP)]
        soul: PathBuf,
        /// Only the notes on this entry.
        id: Option<String>,
    },
    /// Delete one of a companion's notes, the only way a note goes.
    ///
    /// A note it released stays in the entry's ledger; strike that line too.
    #[command(after_long_help = "Example:\n  clanka lore forget ~/.config/clanka/souls/<id>.md 12")]
    Forget {
        #[arg(help = crate::SOUL_HELP)]
        soul: PathBuf,
        /// The note's number, as `clanka lore notes` prints it.
        note: i64,
    },
    /// Hide a companion's line from every reader: one line, or everything one companion
    /// added since a time.
    ///
    /// The line stays in the ledger, with your strike after it.
    #[command(after_long_help = "Example:\n  \
        clanka lore strike moonrot 3fa9c2d1\n  \
        clanka lore strike --by ~/.config/clanka/souls/<id>.md --since 2d")]
    Strike {
        /// The entry, then the line's ID as `clanka lore show` prints it.
        #[arg(num_args = 0..=2, value_names = ["ID", "LINE"])]
        line: Vec<String>,
        /// Strike every line this companion added, in every entry.
        #[arg(long, value_name = "SOUL", requires = "since", conflicts_with = "line")]
        by: Option<PathBuf>,
        /// With --by: from then on (45m, 6h, 2d ago, or a UTC time).
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,
    },
    /// Fold an entry's lines into a new digest now, on a companion's model.
    ///
    /// The keeper does this on its own after a night once an entry has more than 20 lines
    /// since its last digest. The digest is written in no companion's voice and checked
    /// like the keeper's: every name and number in it must be in the entry or a line it cites.
    #[command(
        after_long_help = "Example:\n  clanka lore redigest ~/.config/clanka/souls/<id>.md moonrot"
    )]
    Redigest {
        #[arg(help = crate::SOUL_HELP)]
        soul: PathBuf,
        id: String,
    },
}

impl LoreCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::New { .. } => "lore new",
            Self::Edit { .. } => "lore edit",
            Self::Set { .. } => "lore set",
            Self::List => "lore list",
            Self::Show { .. } => "lore show",
            Self::Lint => "lore lint",
            Self::Notes { .. } => "lore notes",
            Self::Forget { .. } => "lore forget",
            Self::Strike { .. } => "lore strike",
            Self::Redigest { .. } => "lore redigest",
        }
    }
}

/// Returns false when `lint` found an error.
pub fn run(paths: &Paths, command: LoreCommand) -> Result<bool> {
    let config = Config::load(paths)?;
    let lore = config.lore(paths);
    match command {
        LoreCommand::New { id, title } => new(&lore, &id, title)?,
        LoreCommand::Edit { id } => edit(&lore, &id)?,
        LoreCommand::Set { id, file } => {
            let text = if file == Path::new("-") {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                text
            } else {
                std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?
            };
            lore.write_entry(&id, &text)?;
            println!("Wrote {}.", lore.entry_path(&id).display());
        }
        LoreCommand::List => list(&lore)?,
        LoreCommand::Show { id } => show(&lore, &id)?,
        LoreCommand::Lint => return lint(paths, &lore),
        LoreCommand::Notes { soul, id } => {
            let companion = setup::open(paths, &soul, SharedEmbedder::load(paths)?, None)?;
            let notes = companion.lore_notes(id.as_deref())?;
            if notes.is_empty() {
                println!("{} keeps no notes on that.", companion.soul().traits.name);
            }
            for note in notes {
                let released = match &note.released {
                    Some(line) => format!(", released as [{}]", &line[line.len() - 8..]),
                    None => String::new(),
                };
                println!(
                    "{:>4}  {}  {} ({}{released})",
                    note.id,
                    note.entry,
                    note.text,
                    note.provenance.as_str()
                );
            }
        }
        LoreCommand::Forget { soul, note } => {
            let mut companion = setup::open(paths, &soul, SharedEmbedder::load(paths)?, None)?;
            let gone = companion.forget_lore_note(note)?;
            println!("Forgot note {note} on {}: {}", gone.entry, gone.text);
            if let Some(line) = gone.released {
                println!(
                    "It was released; strike it from the ledger too: clanka lore strike {} {}",
                    gone.entry,
                    &line[line.len() - 8..]
                );
            }
        }
        LoreCommand::Strike { line, by, since } => match (by, &line[..]) {
            (Some(soul), _) => strike_by(&lore, &soul, since.as_deref().unwrap_or_default())?,
            (None, [id, reference]) => {
                let struck = lore.strike(id, reference, now())?;
                println!("Struck [{}] from {id}: {}", struck.short(), struck.text);
            }
            _ => bail!("give the entry and the line (clanka lore strike <id> <line>), or --by"),
        },
        LoreCommand::Redigest { soul, id } => {
            let Some(model) = &config.model else {
                bail!(
                    "no companion model configured to write the digest; add a [model] section to {}",
                    paths.config_file().display()
                );
            };
            let mut companion =
                setup::open(paths, &soul, SharedEmbedder::load(paths)?, Some(model))?;
            if companion.redigest(&id)? {
                println!("{id} has a new digest.");
            } else {
                println!(
                    "No digest: {id} has no lines to fold, is sealed, or the digest was refused."
                );
            }
        }
    }
    Ok(true)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn new(lore: &Lore, id: &str, title: Option<String>) -> Result<()> {
    if !valid_id(id) {
        bail!("{id:?} is not an entry ID: segments of a-z, 0-9 and -, separated by /");
    }
    let path = lore.entry_path(id);
    if path.exists() {
        bail!(
            "{} exists already; edit it with `clanka lore edit {id}`",
            path.display()
        );
    }
    // A stub a companion made keeps its title.
    let title = match title {
        Some(title) => title,
        None => match lore.entry(id)? {
            Some(stub) => stub.front.title,
            None => id.rsplit('/').next().unwrap_or(id).replace('-', " "),
        },
    };
    std::fs::create_dir_all(&lore.dir)?;
    let git = lore.git_init();
    let title = toml::Value::String(title).to_string();
    lore.write_entry(id, &format!("+++\ntitle = {title}\naliases = []\n+++\n\n"))?;
    println!("Write the entry: {}", path.display());
    println!("Then grant it in a soul.md's frontmatter: lore = [\"{id}\"]");
    if !git {
        println!("git is not installed, so changes to the lore are not kept on record.");
    }
    Ok(())
}

fn edit(lore: &Lore, id: &str) -> Result<()> {
    let path = lore.entry_path(id);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {}; start it with `clanka lore new {id}`",
            path.display()
        )
    })?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let scratch = std::env::temp_dir().join(format!(
        "clanka-lore-{}-{}.md",
        id.replace('/', "-"),
        std::process::id()
    ));
    std::fs::write(&scratch, &text)?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&scratch)
        .status()
        .with_context(|| format!("running {editor}"))?;
    let edited = std::fs::read_to_string(&scratch)?;
    if !status.success() {
        let _ = std::fs::remove_file(&scratch);
        bail!("{editor} failed; {} is unchanged", path.display());
    }
    if let Err(e) = parse_entry(&edited) {
        bail!(
            "{e}; {} is unchanged and your edit is in {}",
            path.display(),
            scratch.display()
        );
    }
    let _ = std::fs::remove_file(&scratch);
    if edited == text {
        println!("No change.");
        return Ok(());
    }
    lore.write_entry(id, &edited)?;
    println!("Wrote {}.", path.display());
    Ok(())
}

fn list(lore: &Lore) -> Result<()> {
    let ids = lore.ids()?;
    if ids.is_empty() {
        println!(
            "No lore in {}. Start an entry with `clanka lore new <id>`.",
            lore.dir.display()
        );
    }
    for id in ids {
        match lore.entry(&id) {
            Ok(Some(entry)) => {
                let stub = if entry.stub { ", a stub" } else { "" };
                let sealed = if entry.front.sealed { ", sealed" } else { "" };
                println!(
                    "{id}: {} ({} lines standing{stub}{sealed})",
                    entry.front.title,
                    entry.unfolded()
                );
            }
            Ok(None) => {}
            Err(e) => println!("{id}: {e}"),
        }
    }
    Ok(())
}

fn label(line: &Line) -> &'static str {
    line.provenance.map_or("", Provenance::as_str)
}

fn show(lore: &Lore, id: &str) -> Result<()> {
    let Some(entry) = lore.entry(id)? else {
        bail!("there is no entry {id:?} in {}", lore.dir.display());
    };
    println!("# {} ({id})", entry.front.title);
    if entry.stub {
        println!("A stub: nobody has written it yet. `clanka lore new {id}` starts it.");
    } else {
        println!("{}", entry.text);
    }
    let view = entry.view();
    for (deed, quote) in &view.anchored {
        println!(
            "\nNo longer so: \"{quote}\" [{}] {}",
            deed.short(),
            deed.text
        );
    }
    if let Some(digest) = view.digest {
        println!(
            "\nDigest [{}] ({}): {}",
            digest.short(),
            label(digest),
            digest.text
        );
    }
    if !view.lines.is_empty() {
        println!("\nLines:");
    }
    for line in &view.lines {
        println!(
            "[{}] {:?} by {} ({}): {}",
            line.short(),
            line.kind,
            line.name,
            label(line),
            line.text
        );
    }
    let struck = entry
        .lines
        .iter()
        .filter(|l| l.kind == Kind::Strike)
        .count();
    if struck > 0 || entry.bad_lines > 0 {
        println!(
            "\n{struck} struck, {} unreadable ledger lines.",
            entry.bad_lines
        );
    }
    Ok(())
}

fn strike_by(lore: &Lore, soul: &Path, since: &str) -> Result<()> {
    let soul = Soul::load(soul)?;
    let me = soul.traits.id.to_string();
    let from = crate::doctor::when(since, now())?;
    let mut struck = 0;
    for id in lore.ids()? {
        let Some(entry) = lore.entry(&id)? else {
            continue;
        };
        let view = entry.view();
        let mine: Vec<&Line> = view
            .lines
            .iter()
            .copied()
            .chain(view.digest)
            .filter(|l| l.by == me && l.at >= from)
            .collect();
        let strikes: Vec<Line> = mine
            .iter()
            .map(|l| {
                let mut strike = Line::new(Kind::Strike, "user", "", now());
                strike.line = Some(l.id);
                strike
            })
            .collect();
        lore.append(&id, &strikes, None)?;
        struck += strikes.len();
    }
    println!("Struck {struck} lines {} added.", soul.traits.name);
    Ok(())
}

/// Every problem in the lore and the souls' grants; false on an error.
fn lint(paths: &Paths, lore: &Lore) -> Result<bool> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let ids = lore.ids()?;
    for path in lore.files()? {
        match lore.id_of(&path) {
            Some(id) if valid_id(&id) => {}
            _ => errors.push(format!(
                "{}: the name is no entry ID (segments of a-z, 0-9 and -)",
                path.display()
            )),
        }
    }
    for id in &ids {
        match lore.entry(id) {
            Err(e) => errors.push(e.to_string()),
            Ok(None) => {}
            Ok(Some(entry)) => {
                if entry.bad_lines > 0 {
                    errors.push(format!(
                        "{}: {} lines are not ledger lines and are skipped",
                        lore.ledger_path(id).display(),
                        entry.bad_lines
                    ));
                }
                for open in entry.front.opens.iter().filter(|o| !ids.contains(o)) {
                    warnings.push(format!("{id}: opens {open:?}, which is no entry"));
                }
            }
        }
    }
    for soul in paths.soul_files()? {
        match Soul::load(&soul) {
            Ok(s) => warnings.extend(
                missing_grants(&s, &ids)
                    .into_iter()
                    .map(|w| format!("{}: {w}", soul.display())),
            ),
            Err(e) => errors.push(format!("{}: {e}", soul.display())),
        }
    }
    if !lore.dir.exists() {
        warnings.push(format!("{} does not exist yet", lore.dir.display()));
    } else if !lore.has_git() {
        warnings.push(format!(
            "{} is not a git repository, or git is not installed: changes to the lore are not \
             kept on record",
            lore.dir.display()
        ));
    }
    for e in &errors {
        println!("error: {e}");
    }
    for w in &warnings {
        println!("warning: {w}");
    }
    if errors.is_empty() && warnings.is_empty() {
        println!("{} entries, all well.", ids.len());
    }
    Ok(errors.is_empty())
}

/// A soul's grants that name no entry here: a warning, since the entry may be
/// on another machine.
pub fn missing_grants(soul: &Soul, ids: &[String]) -> Vec<String> {
    soul.traits
        .lore
        .iter()
        .filter(|g| !ids.iter().any(|id| clanka_core::lore::grants(g, id)))
        .map(|g| format!("lore grants {g:?}, which names no entry here"))
        .collect()
}
