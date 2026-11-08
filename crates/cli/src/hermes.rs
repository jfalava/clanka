//! `clanka hermes`: a companion as a Hermes profile, and Nightshade, the agent
//! that interviews you and makes one. Both drive the `hermes` CLI, so they
//! work the same from a shell and from an agent's terminal tool.

use anyhow::{Context, Result, bail};
use clanka_core::soul::Soul;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const EXTENSION: &str = "_clanka.abi3.so";
const CLI_HINT: &str = "Plain terminal: markdown does not render, so write plain text.";
const TELEGRAM_HINT: &str =
    "You are on Telegram: short messages, *bold* and _italic_ render, nothing fancier.";
/// Every platform Hermes has a toolset key for; a companion offers clanka's
/// tools on all of them.
const PLATFORMS: [&str; 20] = [
    "cli",
    "telegram",
    "discord",
    "slack",
    "whatsapp",
    "whatsapp_cloud",
    "signal",
    "bluebubbles",
    "email",
    "mattermost",
    "matrix",
    "dingtalk",
    "feishu",
    "wecom",
    "wecom_callback",
    "weixin",
    "qqbot",
    "yuanbao",
    "webhook",
    "api_server",
];

/// Hermes's root directory: `HERMES_HOME`, or `~/.hermes`. `hermes -p <profile>`
/// exports `HERMES_HOME` as that profile's directory for everything it runs, an
/// agent's terminal included, so a `profiles/<name>` suffix is stripped: the
/// shared plugins and the profiles live in the root above it.
pub fn home() -> Result<PathBuf> {
    match std::env::var_os("HERMES_HOME") {
        Some(home) => Ok(root(PathBuf::from(home))),
        None => Ok(directories::BaseDirs::new()
            .context("no home directory to find Hermes in")?
            .home_dir()
            .join(".hermes")),
    }
}

fn root(home: PathBuf) -> PathBuf {
    let mut dir = home.as_path();
    while let Some(parent) = dir.parent() {
        if parent.file_name() == Some(OsStr::new("profiles"))
            && let Some(root) = parent.parent()
        {
            return root.to_owned();
        }
        dir = parent;
    }
    home
}

/// Runs `hermes [-p profile] args…` against `home`, quietly; its output is shown
/// only when it fails.
fn hermes(home: &Path, profile: Option<&str>, args: &[&str]) -> Result<()> {
    let mut command = Command::new("hermes");
    if let Some(profile) = profile {
        command.args(["-p", profile]);
    }
    let output = command
        .args(args)
        .env("HERMES_HOME", home)
        .stdin(Stdio::null())
        .output()
        .context("running hermes; is it on PATH?")?;
    if !output.status.success() {
        bail!(
            "hermes {} failed:\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// `hermes [-p profile] config get key --json --raw`: None when the key is not
/// set.
fn config_get(home: &Path, profile: Option<&str>, key: &str) -> Result<Option<serde_json::Value>> {
    let mut command = Command::new("hermes");
    if let Some(profile) = profile {
        command.args(["-p", profile]);
    }
    let output = command
        .args(["config", "get", key, "--json", "--raw"])
        .env("HERMES_HOME", home)
        .stdin(Stdio::null())
        .output()
        .context("running hermes; is it on PATH?")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        // Hermes has printed this on stdout and, in later versions, on stderr.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stdout.contains("Config key not set") || stderr.contains("Config key not set") {
            return Ok(None);
        }
        bail!("hermes config get {key} failed:\n{stdout}{stderr}");
    }
    let json = stdout.lines().last().unwrap_or_default();
    Ok(Some(serde_json::from_str(json).with_context(|| {
        format!("hermes config get {key} did not print JSON: {json}")
    })?))
}

/// Gives the profile the root's own model endpoints: its `providers:` dict,
/// the aliases that name them and the reasoning effort each model wants, and
/// the secrets they read. `hermes -p` loads only the profile's .env and drops
/// keys inherited from the shell, so a provider the root reaches is unreachable
/// from a profile until its key is in the profile's .env too. Nothing is
/// removed from the profile and existing keys are kept.
fn share_providers(home: &Path, profile: &str, dir: &Path) -> Result<()> {
    let Some(providers) = config_get(home, None, "providers")? else {
        return Ok(());
    };
    let Some(providers) = providers.as_object().filter(|p| !p.is_empty()) else {
        return Ok(());
    };
    for (key, value) in [
        (
            "providers",
            Some(serde_json::Value::Object(providers.clone())),
        ),
        ("model.aliases", config_get(home, None, "model.aliases")?),
        (
            "agent.reasoning_overrides",
            config_get(home, None, "agent.reasoning_overrides")?,
        ),
    ] {
        let Some(root) = value.as_ref().and_then(|v| v.as_object()) else {
            continue;
        };
        // The root's entries win over the profile's, and the profile keeps any of
        // its own. Hermes replaces a non-empty section only with --force.
        let mut merged = config_get(home, Some(profile), key)?
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let before = merged.clone();
        merged.extend(root.clone());
        if merged != before {
            let merged = serde_json::Value::Object(merged).to_string();
            hermes(
                home,
                Some(profile),
                &["config", "set", "--force", key, &merged],
            )?;
        }
    }
    let key_envs: Vec<&str> = providers
        .values()
        .filter_map(|p| p.get("key_env")?.as_str())
        .collect();
    if key_envs.is_empty() {
        return Ok(());
    }
    let root_env = std::fs::read_to_string(home.join(".env")).unwrap_or_default();
    let env_path = dir.join(".env");
    let mut env = std::fs::read_to_string(&env_path).unwrap_or_default();
    let (lines, missing) = missing_env_lines(&root_env, &env, &key_envs);
    for name in missing {
        eprintln!(
            "warning: {name} is not in {}; its provider will not work from the {profile} profile",
            home.join(".env").display()
        );
    }
    if lines.is_empty() {
        return Ok(());
    }
    if !env.is_empty() && !env.ends_with('\n') {
        env.push('\n');
    }
    for line in lines {
        env.push_str(line);
        env.push('\n');
    }
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&env_path)
        .with_context(|| format!("writing {}", env_path.display()))?;
    std::io::Write::write_all(&mut file, env.as_bytes())?;
    Ok(())
}

/// The `NAME=value` lines of `root_env` for the `names` that `profile_env`
/// lacks, and the names neither file has.
fn missing_env_lines<'a>(
    root_env: &'a str,
    profile_env: &str,
    names: &[&'a str],
) -> (Vec<&'a str>, Vec<&'a str>) {
    fn line<'e>(env: &'e str, name: &str) -> Option<&'e str> {
        env.lines().find(|line| {
            line.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('='))
        })
    }
    let mut lines = Vec::new();
    let mut missing = Vec::new();
    for &name in names {
        if line(profile_env, name).is_some() {
            continue;
        }
        match line(root_env, name) {
            Some(line) => lines.push(line),
            None => missing.push(name),
        }
    }
    (lines, missing)
}

/// Creates the profile unless it exists. A directory without a config.yaml is
/// not a profile but stops Hermes from making one, so an empty one is removed.
fn create_profile(home: &Path, profile: &str) -> Result<PathBuf> {
    let dir = home.join("profiles").join(profile);
    if dir.is_dir() && !dir.join("config.yaml").exists() {
        std::fs::remove_dir(&dir)
            .with_context(|| format!("{} is not a profile; remove it", dir.display()))?;
    }
    if !dir.join("config.yaml").exists() {
        hermes(home, None, &["profile", "create", profile])?;
    }
    Ok(dir)
}

/// Makes a Hermes profile that is one companion and nothing else: clanka's two
/// tools are the only toolset on every platform; Hermes's own memory, user
/// profile, coding brief, toolchain probe and agent-style guidance are off; the
/// terminal and Telegram notes are a line each; and SOUL.md is the persona. The
/// root's own providers, model aliases and their keys are shared with it.
/// Safe to run again, and the way to refresh SOUL.md after editing the persona.
pub fn profile(profile: &str, soul_path: &Path) -> Result<()> {
    let soul = Soul::load(soul_path)?;
    let soul_path = soul_path
        .canonicalize()
        .with_context(|| format!("resolving {}", soul_path.display()))?;
    let home = home()?;
    let source = home.join("plugins").join("clanka");
    if !source.join(EXTENSION).is_file() {
        bail!(
            "no clanka plugin at {}; run hermes/install.sh first",
            source.display()
        );
    }
    let dir = create_profile(&home, profile)?;
    // A profile sees only its own plugins directory. The extension is renamed
    // into place: a running gateway keeps its mapped copy of the old file, and
    // rewriting that in place would crash it on its next call.
    let plugin = dir.join("plugins").join("clanka");
    std::fs::create_dir_all(&plugin)?;
    for file in ["plugin.yaml", "__init__.py"] {
        std::fs::copy(source.join(file), plugin.join(file))?;
    }
    let staged = plugin.join(".clanka.so.new");
    std::fs::copy(source.join(EXTENSION), &staged)?;
    std::fs::rename(&staged, plugin.join(EXTENSION))?;

    share_providers(&home, profile, &dir)?;
    let h = |args: &[&str]| hermes(&home, Some(profile), args);
    h(&["plugins", "enable", "clanka"])?;
    let soul_str = soul_path
        .to_str()
        .context("the soul path is not valid UTF-8")?;
    h(&[
        "config",
        "set",
        "plugins.entries.clanka.settings.soul",
        soul_str,
    ])?;
    for platform in PLATFORMS {
        h(&[
            "config",
            "set",
            &format!("platform_toolsets.{platform}"),
            "[clanka]",
        ])?;
    }
    for (key, value) in [
        ("tools.tool_search.enabled", "off"),
        ("memory.memory_enabled", "false"),
        ("memory.user_profile_enabled", "false"),
        ("agent.coding_context", "off"),
        ("agent.environment_probe", "false"),
        ("agent.task_completion_guidance", "false"),
        ("agent.parallel_tool_call_guidance", "false"),
        ("agent.bot_mode_protocol", "false"),
        ("platform_hints.cli.replace", CLI_HINT),
        ("platform_hints.telegram.replace", TELEGRAM_HINT),
    ] {
        h(&["config", "set", key, value])?;
    }
    std::fs::write(dir.join("SOUL.md"), format!("{}\n", soul.persona()))?;
    println!(
        "Profile {profile} is {}: hermes -p {profile} chat",
        soul.traits.name
    );
    Ok(())
}

/// Makes the Nightshade profile: an agent with the terminal and file tools and
/// nothing else, whose SOUL.md is hermes/nightshade/SOUL.md with the path of
/// this binary filled in, so it can run `clanka` to write, lint and wire a
/// companion. The root's own providers, model aliases and their keys are shared
/// with it. Safe to run again. Without `model` the Hermes default stays.
pub fn nightshade(profile: &str, model: Option<&str>) -> Result<()> {
    const SOUL: &str = include_str!("../../../hermes/nightshade/SOUL.md");
    let clanka = std::env::current_exe().context("finding this binary's path")?;
    let clanka = clanka
        .to_str()
        .context("this binary's path is not valid UTF-8")?;
    let home = home()?;
    let dir = create_profile(&home, profile)?;
    share_providers(&home, profile, &dir)?;
    let h = |args: &[&str]| hermes(&home, Some(profile), args);
    if let Some(model) = model {
        h(&["config", "set", "model.default", model])?;
    }
    for platform in ["cli", "telegram", "discord", "slack", "whatsapp", "signal"] {
        h(&[
            "config",
            "set",
            &format!("platform_toolsets.{platform}"),
            "[terminal, file]",
        ])?;
    }
    for (key, value) in [
        ("tools.tool_search.enabled", "off"),
        ("memory.memory_enabled", "false"),
        ("memory.user_profile_enabled", "false"),
        ("agent.coding_context", "off"),
        ("agent.task_completion_guidance", "false"),
        ("agent.parallel_tool_call_guidance", "false"),
        ("platform_hints.cli.replace", CLI_HINT),
        ("platform_hints.telegram.replace", TELEGRAM_HINT),
    ] {
        h(&["config", "set", key, value])?;
    }
    std::fs::write(dir.join("SOUL.md"), SOUL.replace("@CLANKA@", clanka))?;
    println!("Profile {profile} is Nightshade: hermes -p {profile} chat");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_directory_resolves_to_the_root_above_it() {
        assert_eq!(
            root(PathBuf::from("/h/.hermes/profiles/nightshade")),
            PathBuf::from("/h/.hermes")
        );
        assert_eq!(
            root(PathBuf::from("/h/.hermes")),
            PathBuf::from("/h/.hermes")
        );
        assert_eq!(
            root(PathBuf::from("/srv/profiles")),
            PathBuf::from("/srv/profiles")
        );
    }

    #[test]
    fn a_profile_gets_the_root_keys_it_lacks() {
        let root = "A_KEY=1\nB_KEY=2\nB_KEY_2=x\n";
        let profile = "# comment\nB_KEY=mine\n";
        let (lines, missing) =
            missing_env_lines(root, profile, &["A_KEY", "B_KEY", "C_KEY", "B_KEY_"]);
        assert_eq!(lines, ["A_KEY=1"]);
        assert_eq!(missing, ["C_KEY", "B_KEY_"]);
    }

    #[test]
    fn nightshade_soul_has_only_the_clanka_placeholder() {
        let soul = include_str!("../../../hermes/nightshade/SOUL.md");
        assert!(soul.contains("@CLANKA@"));
        assert!(!soul.contains("@ROOT@"));
    }
}
