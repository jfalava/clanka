//! `clanka upgrade`: this binary, and the Hermes plugin when it is installed,
//! replaced by the latest GitHub release once its archive matches the
//! release's SHA256SUMS.

use crate::hermes;
use anyhow::{Context, Result, bail};
use clanka::status;
use std::path::Path;
use std::process::Command;

const REPO: &str = "jfalava/clanka";

/// The target the release archive for this machine is named after. Releases
/// are built for these three only.
const TARGET: Option<&str> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("x86_64-unknown-linux-gnu")
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    Some("aarch64-unknown-linux-gnu")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("aarch64-apple-darwin")
} else {
    None
};

pub fn run(restart: bool) -> Result<()> {
    let Some(target) = TARGET else {
        bail!(
            "clanka is released for Linux on x86_64 or arm64 and macOS on Apple Silicon only; \
             build it from source here"
        );
    };
    let current = env!("CARGO_PKG_VERSION");
    status::phase("Looking for a new release");
    let tag = latest_tag()?;
    let latest = tag.strip_prefix('v').unwrap_or(&tag);
    if !newer(latest, current)? {
        status::quiet();
        if latest == current {
            println!("clanka {current} is the latest release.");
        } else {
            println!("clanka {current} is newer than the latest release, {latest}.");
        }
        return Ok(());
    }
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .context("finding this binary's path")?;
    let staging =
        std::env::temp_dir().join(format!("clanka-upgrade-{}", uuid::Uuid::now_v7().simple()));
    std::fs::create_dir_all(&staging)?;
    let installed = install(&tag, target, &staging, &exe);
    let _ = std::fs::remove_dir_all(&staging);
    let plugin = installed?;
    clanka::ui::success(&format!(
        "clanka {current} is now {latest}: {}",
        exe.display()
    ));
    status::summary(&format!("Upgraded to {latest}"));
    if plugin {
        // The new build refreshes the profiles: what a profile gets may have
        // changed with it.
        status::quiet();
        let mut refresh = Command::new(&exe);
        refresh.args(["hermes", "refresh"]);
        if !restart {
            refresh.arg("--no-restart");
        }
        if !refresh
            .status()
            .context("running clanka hermes refresh")?
            .success()
        {
            bail!(
                "`clanka hermes refresh` failed, so profiles may run the old plugin; run it \
                 again once the cause is fixed"
            );
        }
    }
    Ok(())
}

/// Downloads the release archive, checks it against the release's SHA256SUMS,
/// unpacks it into `staging` and puts its files in place: the binary at `exe`,
/// then the Hermes plugin and the soul-writer skill where they are installed.
/// Says whether the plugin was.
fn install(tag: &str, target: &str, staging: &Path, exe: &Path) -> Result<bool> {
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let name = format!("clanka-{target}.tar.gz");
    let sums = get(&format!("{base}/SHA256SUMS"))?;
    let Some(sha256) = checksum(&sums, &name) else {
        bail!("the SHA256SUMS of {tag} lists no {name}");
    };
    let archive = staging.join(&name);
    crate::download(&format!("{base}/{name}"), &archive, sha256)?;
    status::quiet();
    let files = staging.join("release");
    std::fs::create_dir(&files)?;
    let unpacked = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&files)
        .status()
        .context("running tar")?;
    if !unpacked.success() {
        bail!("tar could not unpack {name}");
    }

    replace(&files.join("clanka"), exe).with_context(|| {
        format!(
            "replacing {}; if something else installed it there, upgrade it that way",
            exe.display()
        )
    })?;

    let plugin = hermes::home()?.join("plugins").join("clanka");
    let has_plugin = plugin.join(hermes::EXTENSION).is_file();
    if has_plugin {
        for file in ["plugin.yaml", "__init__.py", hermes::EXTENSION] {
            replace(&files.join("hermes/clanka").join(file), &plugin.join(file))?;
        }
        clanka::ui::success(&format!("The Hermes plugin in {} is new", plugin.display()));
    }
    let skills = match std::env::var_os("AGENT_SKILLS_HOME") {
        Some(skills) => skills.into(),
        None => directories::BaseDirs::new()
            .context("no home directory")?
            .home_dir()
            .join(".agents/skills"),
    };
    let skill = skills.join("clanka-soul-writer/SKILL.md");
    if skill.is_file() {
        replace(&files.join("skills/soul-writer/SKILL.md"), &skill)?;
    }
    Ok(has_plugin)
}

/// Puts a copy of `new` at `path` by renaming it over the old file. Whatever
/// runs the old one keeps it: a binary or extension rewritten in place
/// crashes what has it mapped, and on macOS fails its code signature.
fn replace(new: &Path, path: &Path) -> Result<()> {
    let file_name = path.file_name().context("no file name")?.to_string_lossy();
    let staged = path.with_file_name(format!(".{file_name}.new"));
    std::fs::copy(new, &staged).with_context(|| format!("writing {}", staged.display()))?;
    std::fs::rename(&staged, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// The tag of the latest release, such as `v0.2.0`.
fn latest_tag() -> Result<String> {
    let release = get(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))
    .context("asking GitHub for the latest clanka release")?;
    let release: serde_json::Value = serde_json::from_str(&release)?;
    release["tag_name"]
        .as_str()
        .map(str::to_owned)
        .context("GitHub's latest clanka release has no tag")
}

fn get(url: &str) -> Result<String> {
    ureq::get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", concat!("clanka/", env!("CARGO_PKG_VERSION")))
        .call()
        .and_then(|mut response| response.body_mut().read_to_string())
        .with_context(|| url.to_owned())
}

/// Whether `latest` is a later version than `current`, both `x.y.z`.
fn newer(latest: &str, current: &str) -> Result<bool> {
    let parse = |version: &str| -> Result<Vec<u64>> {
        version
            .split('.')
            .map(|part| {
                part.parse()
                    .with_context(|| format!("{version} is not a version"))
            })
            .collect()
    };
    Ok(parse(latest)? > parse(current)?)
}

/// The SHA-256 that `sums`, as `sha256sum` writes them, lists for `file`.
fn checksum<'a>(sums: &'a str, file: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (sha256, name) = line.split_once(char::is_whitespace)?;
        // `*` marks a file hashed in binary mode.
        let name = name.trim_start().trim_start_matches('*');
        (name == file).then_some(sha256)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.10.0", "0.9.3").unwrap());
        assert!(newer("1.0.0", "0.99.99").unwrap());
        assert!(!newer("0.1.0", "0.1.0").unwrap());
        assert!(!newer("0.1.0", "0.2.0").unwrap());
        assert!(newer("0.2", "0.1.9").is_ok());
        assert!(newer("v0.2.0", "0.1.0").is_err());
    }

    #[test]
    fn the_checksum_is_the_one_for_that_exact_file() {
        let sums = "aaa  clanka-aarch64-unknown-linux-gnu.tar.gz\n\
                    bbb *clanka-x86_64-unknown-linux-gnu.tar.gz\n";
        assert_eq!(
            checksum(sums, "clanka-x86_64-unknown-linux-gnu.tar.gz"),
            Some("bbb")
        );
        assert_eq!(
            checksum(sums, "clanka-aarch64-unknown-linux-gnu.tar.gz"),
            Some("aaa")
        );
        assert_eq!(checksum(sums, "clanka-aarch64-apple-darwin.tar.gz"), None);
        assert_eq!(checksum(sums, "unknown-linux-gnu.tar.gz"), None);
    }
}
