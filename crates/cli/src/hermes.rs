//! `clanka hermes`: a companion as a Hermes profile, Nightshade, the agent
//! that interviews you and makes one, and the refresh that brings both up to a
//! new build. All drive the `hermes` CLI, so they work the same from a shell
//! and from an agent's terminal tool.

use anyhow::{Context, Result, bail};
use clanka_core::soul::Soul;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const EXTENSION: &str = "_clanka.abi3.so";
/// Where a companion's profile keeps the path of its soul.md.
const SOUL_SETTING: &str = "plugins.entries.clanka.settings.soul";
const NIGHTSHADE: &str = include_str!("../../../hermes/nightshade/SOUL.md");
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
/// the aliases that name them, the reasoning effort each model wants and what
/// each model can do (`model_overrides`), whether a model is asked to title
/// sessions, and the secrets they read. `hermes -p` loads only the profile's .env and drops
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
    for (key, value, depth) in [
        (
            "providers",
            Some(serde_json::Value::Object(providers.clone())),
            1,
        ),
        ("model.aliases", config_get(home, None, "model.aliases")?, 1),
        (
            "agent.reasoning_overrides",
            config_get(home, None, "agent.reasoning_overrides")?,
            1,
        ),
        // Provider, then model, then setting. Every custom endpoint is the
        // one provider `custom` here, so a profile's own models under it
        // must survive the root's.
        (
            "model_overrides",
            config_get(home, None, "model_overrides")?,
            3,
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
        merge(&mut merged, root, depth);
        if merged != before {
            let merged = serde_json::Value::Object(merged).to_string();
            hermes(
                home,
                Some(profile),
                &["config", "set", "--force", key, &merged],
            )?;
        }
    }
    // Some endpoints refuse the request that asks a model to title a session.
    // `config get` fills in Hermes's defaults, so "on" may not be the root's
    // choice while "off" always is: only "off" is passed on.
    let off = Some(serde_json::Value::Bool(false));
    if config_get(home, None, TITLE_BY_MODEL)? == off
        && config_get(home, Some(profile), TITLE_BY_MODEL)? != off
    {
        hermes(
            home,
            Some(profile),
            &["config", "set", TITLE_BY_MODEL, "false"],
        )?;
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

/// Whether Hermes replaces a session's instant title with one a model writes.
const TITLE_BY_MODEL: &str = "auxiliary.title_generation.model_upgrade_enabled";

/// Puts the root's entries into the profile's, where they win. Maps are merged
/// `depth` levels down; below that a root entry replaces the profile's whole.
fn merge(
    profile: &mut serde_json::Map<String, serde_json::Value>,
    root: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
) {
    use serde_json::Value::Object;
    for (key, value) in root {
        match (profile.get_mut(key), value) {
            (Some(Object(mine)), Object(theirs)) if depth > 1 => merge(mine, theirs, depth - 1),
            _ => {
                profile.insert(key.clone(), value.clone());
            }
        }
    }
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

/// Creates the profile unless it exists, and says whether it did. A directory
/// without a config.yaml is not a profile but stops Hermes from making one, so
/// an empty one is removed.
fn create_profile(home: &Path, profile: &str) -> Result<(PathBuf, bool)> {
    let dir = home.join("profiles").join(profile);
    if dir.is_dir() && !dir.join("config.yaml").exists() {
        std::fs::remove_dir(&dir)
            .with_context(|| format!("{} is not a profile; remove it", dir.display()))?;
    }
    let created = !dir.join("config.yaml").exists();
    if created {
        hermes(home, None, &["profile", "create", profile])?;
    }
    Ok((dir, created))
}

/// What a setup that failed part-way leaves behind. A profile this run created
/// is deleted again: half-made, it would answer as a stock Hermes agent under
/// the companion's name. One that was there before is kept, and running the
/// command again finishes it.
fn or_undo(home: &Path, profile: &str, created: bool, set_up: Result<()>) -> Result<()> {
    let Err(error) = set_up else {
        return Ok(());
    };
    if !created {
        return Err(error.context(format!(
            "the {profile} profile is only partly updated; run this again once the cause is fixed"
        )));
    }
    Err(
        match hermes(home, None, &["profile", "delete", "--yes", profile]) {
            Ok(()) => error.context(format!(
                "setting up the {profile} profile failed, so the new profile was removed again"
            )),
            Err(delete) => error.context(format!(
                "setting up the {profile} profile failed and the half-made profile could not be \
                 removed ({delete:#}); remove it with `hermes profile delete {profile}`"
            )),
        },
    )
}

/// The platforms whose toolsets in the profile's `platform_toolsets` do not
/// list clanka yet: a new profile's stock toolbox, which becomes `[clanka]`
/// alone. A list that already has clanka was set by an earlier run, so it is
/// left as it is and a toolset added to it since (say `image_gen`) stays.
fn without_clanka(toolsets: Option<&serde_json::Value>) -> Vec<&'static str> {
    let has_clanka = |platform: &str| {
        toolsets
            .and_then(|t| t.get(platform)?.as_array())
            .is_some_and(|list| list.iter().any(|t| t == "clanka"))
    };
    PLATFORMS
        .into_iter()
        .filter(|platform| !has_clanka(platform))
        .collect()
}

/// Makes a Hermes profile that is one companion and nothing else: clanka's two
/// tools are the only toolset on every platform, plus any you added to a
/// platform's list after the first run; Hermes's own memory, user
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
    let (dir, created) = create_profile(&home, profile)?;
    or_undo(
        &home,
        profile,
        created,
        set_up_companion(&home, profile, &dir, &source, &soul, &soul_path),
    )?;
    println!(
        "Profile {profile} is {}: hermes -p {profile} chat",
        soul.traits.name
    );
    Ok(())
}

/// Everything `profile` does to the profile once it exists.
fn set_up_companion(
    home: &Path,
    profile: &str,
    dir: &Path,
    source: &Path,
    soul: &Soul,
    soul_path: &Path,
) -> Result<()> {
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

    share_providers(home, profile, dir)?;
    let h = |args: &[&str]| hermes(home, Some(profile), args);
    h(&["plugins", "enable", "clanka"])?;
    let soul_str = soul_path
        .to_str()
        .context("the soul path is not valid UTF-8")?;
    h(&["config", "set", SOUL_SETTING, soul_str])?;
    let toolsets = config_get(home, Some(profile), "platform_toolsets")?;
    for platform in without_clanka(toolsets.as_ref()) {
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
    Ok(())
}

/// Makes the Nightshade profile: an agent with the terminal and file tools and
/// nothing else, whose SOUL.md is hermes/nightshade/SOUL.md with the path of
/// this binary filled in, so it can run `clanka` to write, lint and wire a
/// companion. The root's own providers, model aliases and their keys are shared
/// with it. Safe to run again. Without `model` the Hermes default stays.
pub fn nightshade(profile: &str, model: Option<&str>) -> Result<()> {
    let clanka = std::env::current_exe().context("finding this binary's path")?;
    let clanka = clanka
        .to_str()
        .context("this binary's path is not valid UTF-8")?;
    let home = home()?;
    let (dir, created) = create_profile(&home, profile)?;
    or_undo(
        &home,
        profile,
        created,
        set_up_nightshade(&home, profile, &dir, model, clanka),
    )?;
    println!("Profile {profile} is Nightshade: hermes -p {profile} chat");
    Ok(())
}

/// Everything `nightshade` does to the profile once it exists.
fn set_up_nightshade(
    home: &Path,
    profile: &str,
    dir: &Path,
    model: Option<&str>,
    clanka: &str,
) -> Result<()> {
    share_providers(home, profile, dir)?;
    let h = |args: &[&str]| hermes(home, Some(profile), args);
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
    std::fs::write(dir.join("SOUL.md"), NIGHTSHADE.replace("@CLANKA@", clanka))?;
    Ok(())
}

/// Brings every profile clanka set up to the plugin in the root's
/// `plugins/clanka`, the only copy `hermes/install.sh` replaces: a profile
/// runs its own. A profile with a soul in its clanka settings is a companion's
/// and gets what `profile` does; one whose SOUL.md opens as Nightshade's does
/// gets what `nightshade` does, on the model it has. Any other is left alone.
/// A profile that fails is named and the rest are still done. With `restart`,
/// whatever has the plugin loaded is then restarted, since it runs the build
/// it loaded until it is.
pub fn refresh(restart: bool) -> Result<()> {
    let home = home()?;
    let profiles = home.join("profiles");
    let mut names: Vec<String> = std::fs::read_dir(&profiles)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| profiles.join(name).join("config.yaml").exists())
        .collect();
    names.sort();
    let mut refreshed = 0;
    let mut failed = Vec::new();
    for name in &names {
        let result = config_get(&home, Some(name), SOUL_SETTING).and_then(|soul| {
            match soul.as_ref().and_then(|soul| soul.as_str()) {
                Some(soul) => profile(name, Path::new(soul)).map(|()| true),
                None if is_nightshade(&profiles.join(name)) => {
                    nightshade(name, None).map(|()| true)
                }
                None => Ok(false),
            }
        });
        match result {
            Ok(true) => refreshed += 1,
            Ok(false) => {}
            Err(error) => {
                eprintln!("error: profile {name}: {error:#}");
                failed.push(name.as_str());
            }
        }
    }
    if refreshed == 0 && failed.is_empty() {
        println!(
            "No profile under {} was set up by clanka; \
             `clanka hermes profile <profile> <soul.md>` makes one.",
            profiles.display()
        );
    } else if refreshed > 0 && restart {
        restart_gateway(&home)?;
    } else if refreshed > 0 {
        println!("A gateway that already loaded clanka runs the old build until it restarts.");
    }
    if !failed.is_empty() {
        bail!("could not refresh {}", failed.join(", "));
    }
    Ok(())
}

/// Whether the profile's SOUL.md opens with the line Nightshade's does. A
/// companion of that name has a soul in its settings and is found by that.
fn is_nightshade(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("SOUL.md"))
        .is_ok_and(|soul| soul.lines().next() == NIGHTSHADE.lines().next())
}

/// Restarts what has the plugin loaded. Under systemd that is every running
/// `hermes-gateway*` and `hermes-serve*` user unit (the backend loads the
/// plugin too), restarted by systemctl itself: `hermes gateway restart` there
/// also rewrites the unit file, which a unit Hermes did not install, such as
/// Home Manager's, must be spared. Anywhere else, launchd included, `hermes
/// gateway restart` does it, and asks first while a session is live.
fn restart_gateway(home: &Path) -> Result<()> {
    let units: Vec<String> = Command::new("systemctl")
        .args(["--user", "list-units", "--type=service", "--state=active"])
        .args(["--plain", "--no-legend", "hermes-gateway*", "hermes-serve*"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut command = if units.is_empty() {
        println!("Restarting the gateway: hermes gateway restart");
        let mut command = Command::new("hermes");
        command
            .args(["gateway", "restart"])
            .env("HERMES_HOME", home);
        command
    } else {
        println!("Restarting {}", units.join(" and "));
        let mut command = Command::new("systemctl");
        command.args(["--user", "restart"]).args(&units);
        command
    };
    let status = command.status().context("restarting the gateway")?;
    if !status.success() {
        bail!("the gateway was not restarted ({status}); it still runs the old build");
    }
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
    fn the_roots_entries_win_and_the_profile_keeps_its_own() {
        let object = |v: serde_json::Value| v.as_object().unwrap().clone();
        let root = object(serde_json::json!({
            "custom": { "glm": { "supports_reasoning": false } },
        }));
        let profile = serde_json::json!({
            "custom": {
                "glm": { "supports_reasoning": true, "context_window": 8192 },
                "llava": { "supports_vision": true },
            },
            "openrouter": { "x": {} },
        });

        // Model overrides: only the setting the root names changes.
        let mut deep = object(profile.clone());
        merge(&mut deep, &root, 3);
        assert_eq!(
            serde_json::Value::Object(deep),
            serde_json::json!({
                "custom": {
                    "glm": { "supports_reasoning": false, "context_window": 8192 },
                    "llava": { "supports_vision": true },
                },
                "openrouter": { "x": {} },
            })
        );

        // Providers and aliases: the root's entry replaces the profile's whole.
        let mut shallow = object(profile);
        merge(&mut shallow, &root, 1);
        assert_eq!(
            serde_json::Value::Object(shallow),
            serde_json::json!({
                "custom": { "glm": { "supports_reasoning": false } },
                "openrouter": { "x": {} },
            })
        );
    }

    #[test]
    fn a_rerun_keeps_toolsets_added_next_to_clanka() {
        // A new profile: every platform still has Hermes's stock toolbox.
        assert_eq!(without_clanka(None), PLATFORMS);
        let toolsets = serde_json::json!({
            "cli": ["clanka"],
            "telegram": ["clanka", "image_gen"],
            "discord": ["hermes-discord"],
            "slack": "clanka-ish",
        });
        let reset = without_clanka(Some(&toolsets));
        assert!(!reset.contains(&"cli") && !reset.contains(&"telegram"));
        assert!(reset.contains(&"discord") && reset.contains(&"slack"));
        assert_eq!(reset.len(), PLATFORMS.len() - 2);
    }

    #[test]
    fn nightshade_soul_has_only_the_clanka_placeholder() {
        assert!(NIGHTSHADE.contains("@CLANKA@"));
        assert!(!NIGHTSHADE.contains("@ROOT@"));
    }
}
