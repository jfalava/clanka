//! `clanka hermes profile` against a stand-in for the `hermes` CLI, so no real
//! Hermes is needed or touched.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

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

#[test]
fn hermes_profile_that_fails_removes_only_a_profile_it_made() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("hermes");
    let plugin = home.join("plugins").join("clanka");
    std::fs::create_dir_all(&plugin).unwrap();
    for file in ["plugin.yaml", "__init__.py", "_clanka.abi3.so"] {
        std::fs::write(plugin.join(file), "").unwrap();
    }
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let hermes = bin.join("hermes");
    std::fs::write(&hermes, HERMES).unwrap();
    std::fs::set_permissions(&hermes, std::fs::Permissions::from_mode(0o755)).unwrap();
    let soul = dir.path().join("soul.md");
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
    let mut path = vec![bin];
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let run = || {
        let output = Command::new(env!("CARGO_BIN_EXE_clanka"))
            .args(["hermes", "profile", "nova"])
            .arg(&soul)
            .env("HERMES_HOME", &home)
            .env("HOME", dir.path())
            .env("PATH", std::env::join_paths(&path).unwrap())
            .output()
            .unwrap();
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
