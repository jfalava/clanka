//! Puts the page `clanka web` serves into the binary: every file under
//! `web/dist`, which is committed, as an `include_bytes!` line in
//! `$OUT_DIR/assets.rs`. Building clanka never runs Bun; `bun run --cwd web
//! build` rebuilds the page, and CI fails when the committed one is stale.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());

    let mut files = Vec::new();
    collect(&dist, &mut files);
    files.sort();
    if files.is_empty() {
        println!(
            "cargo:warning={} holds no page, so `clanka web` will have none to serve",
            dist.display()
        );
    }

    let mut table = String::from("pub static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for file in &files {
        let route = file
            .strip_prefix(&dist)
            .expect("collected under dist")
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let bytes = std::fs::read(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        // The page's files keep their names from build to build, so a browser
        // asks whether its copy is still this one.
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let path = file.canonicalize().expect("a file that was just read");
        let _ = writeln!(
            table,
            "    ({:?}, {:?}, include_bytes!({:?})),",
            format!("/{route}"),
            format!("\"{:016x}\"", hasher.finish()),
            path
        );
    }
    table.push_str("];\n");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("assets.rs"), table).expect("writing assets.rs");
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}
