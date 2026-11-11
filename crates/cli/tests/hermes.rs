//! `clanka hermes profile` and `clanka hermes refresh` against a stand-in for
//! the `hermes` CLI, so no real Hermes is needed or touched.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A `hermes` that makes and deletes profiles under `$HERMES_HOME`, has no
/// config set, and cannot enable a plugin.
const HERMES: &str = r#"#!/bin/sh
if [ "$1" = -p ]; then shift 2; fi
case "$1 $2" in
"profile create") mkdir -p "$HERMES_HOME/profiles/$3" && : > "$HERMES_HOME/profiles/$3/config.yaml" ;;
"profile delete") rm -r "$HERMES_HOME/profiles/$4" ;;
"config get") echo "Config key not set"; exit 1 ;;
"plugins enable") echo "no uv.lock" >&2; exit 1 ;;
esac
"#;

/// A `hermes` whose only config is a profile's soul setting, read from the
/// profile's `soul` file, and which notes a gateway restart in
/// `$HERMES_HOME/calls`.
const SETTLED_HERMES: &str = r#"#!/bin/sh
dir=$HERMES_HOME
if [ "$1" = -p ]; then dir=$HERMES_HOME/profiles/$2; shift 2; fi
case "$1 $2" in
"config get")
    [ "$3" = plugins.entries.clanka.settings.soul ] && [ -f "$dir/soul" ] && exec cat "$dir/soul"
    echo "Config key not set"; exit 1 ;;
"gateway restart") echo "hermes gateway restart" >> "$HERMES_HOME/calls" ;;
esac
"#;

/// A `systemctl` that lists the units in `$HERMES_HOME/units`, none without
/// the file, and notes anything else it is asked in `$HERMES_HOME/calls`.
const SYSTEMCTL: &str = r#"#!/bin/sh
case "$*" in
*list-units*) [ -f "$HERMES_HOME/units" ] && cat "$HERMES_HOME/units" ;;
*) echo "systemctl $*" >> "$HERMES_HOME/calls" ;;
esac
"#;

/// A Hermes root under `dir` with the clanka plugin installed, each file
/// holding "new".
fn hermes_home(dir: &Path) -> PathBuf {
    let home = dir.join("hermes");
    let plugin = home.join("plugins").join("clanka");
    std::fs::create_dir_all(&plugin).unwrap();
    for file in ["plugin.yaml", "__init__.py", "_clanka.abi3.so"] {
        std::fs::write(plugin.join(file), "new").unwrap();
    }
    home
}

/// Puts the stand-in `name` in `dir/bin`.
fn stand_in(dir: &Path, name: &str, script: &str) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join(name), script).unwrap();
    std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Nova's soul.md in `dir`.
fn soul(dir: &Path) -> PathBuf {
    let soul = dir.join("soul.md");
    std::fs::write(
        &soul,
        format!(
            "+++\nid = \"{}\"\nname = \"Nova\"\ncontrariness = 0.5\npersistence = 0.5\n\
             time_sense = false\nrecall_window = 2\ndrift = 0.3\ntired_at = 0.7\n\
             sleep_at = 0.9\npushback_window = 20\ndoctor_token = \"0123456789abcdef0123\"\n\
             +++\n# Nova\n",
            uuid::Uuid::now_v7()
        ),
    )
    .unwrap();
    soul
}

/// `clanka hermes args…` with the stand-ins in `dir/bin` first on PATH.
fn clanka_hermes(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut path = vec![dir.join("bin")];
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    Command::new(env!("CARGO_BIN_EXE_clanka"))
        .arg("hermes")
        .args(args)
        .env("HERMES_HOME", home)
        .env("HOME", dir)
        .env("PATH", std::env::join_paths(&path).unwrap())
        .output()
        .unwrap()
}

#[test]
fn hermes_profile_that_fails_removes_only_a_profile_it_made() {
    let dir = tempfile::tempdir().unwrap();
    let home = hermes_home(dir.path());
    stand_in(dir.path(), "hermes", HERMES);
    let soul = soul(dir.path());
    let run = || {
        let output = clanka_hermes(
            dir.path(),
            &home,
            &["profile", "nova", soul.to_str().unwrap()],
        );
        assert!(!output.status.success());
        String::from_utf8(output.stderr).unwrap()
    };
    let profile = home.join("profiles").join("nova");

    // The run made the profile, so the failure takes it away again.
    let error = run();
    assert!(error.contains("no uv.lock"), "{error}");
    assert!(error.contains("removed again"), "{error}");
    assert!(!profile.exists());

    // A profile that was there before stays.
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("config.yaml"), "").unwrap();
    let error = run();
    assert!(error.contains("no uv.lock"), "{error}");
    assert!(error.contains("partly updated"), "{error}");
    assert!(profile.join("config.yaml").exists());
}

#[test]
fn hermes_refresh_updates_the_profiles_clanka_made_and_restarts_what_runs_them() {
    let dir = tempfile::tempdir().unwrap();
    let home = hermes_home(dir.path());
    stand_in(dir.path(), "hermes", SETTLED_HERMES);
    stand_in(dir.path(), "systemctl", SYSTEMCTL);
    let soul = soul(dir.path());
    let read = |path: &Path| std::fs::read_to_string(path).unwrap();
    let profile = |name: &str| {
        let profile = home.join("profiles").join(name);
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("config.yaml"), "").unwrap();
        profile
    };
    // A companion on the build before this one, someone's own profile, and
    // Nightshade under another name with the last build's SOUL.md.
    let nova = profile("nova");
    let extension = nova.join("plugins").join("clanka").join("_clanka.abi3.so");
    std::fs::create_dir_all(extension.parent().unwrap()).unwrap();
    std::fs::write(&extension, "old").unwrap();
    std::fs::write(nova.join("soul"), format!("{:?}\n", soul.to_str().unwrap())).unwrap();
    let other = profile("other");
    std::fs::write(other.join("SOUL.md"), "mine").unwrap();
    let scribe = profile("scribe");
    std::fs::write(scribe.join("SOUL.md"), "# Nightshade\nold").unwrap();
    let calls = home.join("calls");

    let output = clanka_hermes(dir.path(), &home, &["refresh", "--no-restart"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(read(&extension), "new");
    assert!(read(&nova.join("SOUL.md")).starts_with("# Nova"));
    assert_eq!(read(&other.join("SOUL.md")), "mine");
    assert!(!other.join("plugins").exists());
    let nightshade = read(&scribe.join("SOUL.md"));
    assert!(nightshade.starts_with("# Nightshade\n"));
    assert!(nightshade.len() > 1000 && !nightshade.contains("@CLANKA@"));
    assert!(!calls.exists());

    // Without systemd units it is Hermes that restarts the gateway.
    let output = clanka_hermes(dir.path(), &home, &["refresh"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(read(&calls), "hermes gateway restart\n");

    // Units that systemd runs are restarted by systemd, and only those.
    std::fs::write(
        home.join("units"),
        "hermes-gateway.service loaded active running Hermes gateway\n\
         hermes-serve.service   loaded active running Hermes backend\n",
    )
    .unwrap();
    std::fs::remove_file(&calls).unwrap();
    let output = clanka_hermes(dir.path(), &home, &["refresh"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        read(&calls),
        "systemctl --user restart hermes-gateway.service hermes-serve.service\n"
    );

    // A profile whose soul is gone fails the command and stops nothing else.
    let gone = profile("gone");
    std::fs::write(gone.join("soul"), "\"/nowhere/soul.md\"\n").unwrap();
    std::fs::write(&extension, "old").unwrap();
    let output = clanka_hermes(dir.path(), &home, &["refresh", "--no-restart"]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("could not refresh gone"), "{error}");
    assert_eq!(read(&extension), "new");
}
