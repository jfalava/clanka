//! `clanka list`, `export` and `import`: seeing which companions are set up
//! here, and moving one to another machine as a directory of three files.

use anyhow::{Context, Result, bail};
use clanka::setup::{self, Config, Keys, Paths, SharedEmbedder, write_private};
use clanka_core::deps::KeySource;
use clanka_core::soul::Soul;
use std::path::Path;

const SOUL_FILE: &str = "soul.md";
const DB_FILE: &str = "companion.db";
const KEY_FILE: &str = "key";

/// Every companion under the souls directory. Opens no database, so it needs
/// neither keys nor the embedding model.
pub fn list(paths: &Paths) -> Result<()> {
    let souls = paths.soul_files()?;
    if souls.is_empty() {
        println!(
            "No companions in {}. Make one with `clanka init <name>`.",
            paths.souls.display()
        );
        return Ok(());
    }
    for path in souls {
        let soul = match Soul::load(&path) {
            Ok(soul) => soul,
            Err(e) => {
                println!("?  {}\n   does not load: {e}", path.display());
                continue;
            }
        };
        let db = paths.data.join(format!("{}.db", soul.traits.id));
        let memory = match std::fs::metadata(&db) {
            Ok(meta) => format!("{:.1} MB of memory", meta.len() as f64 / 1_048_576.0),
            Err(_) => "never opened".into(),
        };
        println!(
            "{}  {}\n   {}, {memory}",
            soul.traits.name,
            path.display(),
            soul.traits.id
        );
    }
    Ok(())
}

/// Writes the companion to a new directory: its soul.md, a snapshot of its
/// database and the database key.
pub fn export(paths: &Paths, soul_path: &Path, dir: &Path) -> Result<()> {
    if dir.exists() {
        bail!(
            "{} already exists; export to a new directory",
            dir.display()
        );
    }
    let soul = Soul::load(soul_path)?;
    let id = soul.traits.id;
    let key = Keys::new(paths, &Config::load(paths)?).key(id)?;
    let companion = setup::open(paths, soul_path, SharedEmbedder::load(paths)?, None)?;

    let written = (|| -> Result<()> {
        // The key first: it makes the directory, which only you can enter.
        write_private(dir, &dir.join(KEY_FILE), &key)
            .with_context(|| format!("writing {}", dir.join(KEY_FILE).display()))?;
        companion.export_to(&dir.join(DB_FILE))?;
        std::fs::copy(soul_path, dir.join(SOUL_FILE))
            .with_context(|| format!("copying {}", soul_path.display()))?;
        Ok(())
    })();
    // Half an export is no use, and it would leave the key lying in plain text.
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(dir);
        return Err(e.context(format!(
            "the export failed and {} was removed again",
            dir.display()
        )));
    }
    println!(
        "Exported {} ({id}) to {}.\nIt holds the database key in plain text beside the \
         database: anyone who gets the directory can read every memory. Move it over a channel \
         you trust, `clanka import` it there, then delete it.",
        soul.traits.name,
        dir.display()
    );
    Ok(())
}

/// Sets up the companion an export directory holds: its key, its database
/// and, last, its soul.md, so a failure never leaves a soul without memory.
pub fn import(paths: &Paths, dir: &Path) -> Result<()> {
    let read = |name: &str| {
        let path = dir.join(name);
        std::fs::read_to_string(&path).with_context(|| {
            format!(
                "reading {}; is this a `clanka export` directory?",
                path.display()
            )
        })
    };
    let text = read(SOUL_FILE)?;
    let key = read(KEY_FILE)?;
    let soul = Soul::parse(&text).with_context(|| format!("{}/{SOUL_FILE}", dir.display()))?;
    let id = soul.traits.id;
    let source_db = dir.join(DB_FILE);
    if !source_db.is_file() {
        bail!(
            "no {DB_FILE} in {}; is this a `clanka export` directory?",
            dir.display()
        );
    }

    let soul_path = paths.souls.join(format!("{id}.md"));
    let db = paths.data.join(format!("{id}.db"));
    // The write-ahead log counts: SQLite would replay one left behind by a
    // killed process into the imported database.
    let files = [
        soul_path.clone(),
        db.clone(),
        db.with_extension("db-wal"),
        db.with_extension("db-shm"),
    ];
    if let Some(path) = files.iter().find(|path| path.exists()) {
        bail!(
            "{} ({id}) is already here: {} exists. An import never overwrites. To replace the \
             companion, stop everything that has it open (clanka chat, clanka mcp, the Hermes \
             gateway), export it somewhere, then remove {}, {} and any -wal and -shm file \
             beside that database",
            soul.traits.name,
            path.display(),
            soul_path.display(),
            db.display()
        );
    }

    let keys = Keys::new(paths, &Config::load(paths)?);
    keys.store(id, key.trim())?;
    let partial = db.with_extension("db.part");
    // Whatever goes wrong from here, nothing stays behind to block a retry.
    let remove = || {
        for path in files.iter().chain([&partial]) {
            let _ = std::fs::remove_file(path);
        }
        keys.forget(id);
    };
    let placed = (|| -> Result<()> {
        std::fs::create_dir_all(&paths.data)?;
        std::fs::create_dir_all(&paths.souls)?;
        std::fs::copy(&source_db, &partial).with_context(|| format!("copying {DB_FILE}"))?;
        std::fs::rename(&partial, &db)?;
        std::fs::write(&soul_path, &text)
            .with_context(|| format!("writing {}", soul_path.display()))
    })();
    if let Err(e) = placed {
        remove();
        return Err(e.context(format!(
            "the import failed and what it had placed was removed again; {} is untouched",
            dir.display()
        )));
    }

    // Opening it is the check that the key fits and the copy is whole. That
    // needs the embedding model; without it the import itself is still fine.
    let unchecked = |e: &anyhow::Error| format!("\nNot opened to check it: {e:#}.");
    let note = match SharedEmbedder::load(paths) {
        Err(e) => unchecked(&e),
        Ok(embedder) => match setup::open(paths, &soul_path, embedder, None) {
            Ok(_) => String::new(),
            Err(e) => match e.downcast_ref::<clanka_core::Error>() {
                Some(clanka_core::Error::EmbedderChanged { stored, .. }) => format!(
                    "\nIts vectors come from another embedding model ({stored}): run `clanka \
                     doctor reembed {}` before talking with it.",
                    soul_path.display()
                ),
                // Wrong key, a broken copy, a database newer than this clanka.
                Some(_) => {
                    remove();
                    return Err(e.context(format!(
                        "the imported companion does not open, so it was removed again; {} is \
                         untouched",
                        dir.display()
                    )));
                }
                // A setting in config.toml, not the import.
                None => unchecked(&e),
            },
        },
    };
    println!(
        "Imported {} ({id}): {}{note}\nDelete {} once it works: it holds the key in plain text.",
        soul.traits.name,
        soul_path.display(),
        dir.display()
    );
    Ok(())
}
