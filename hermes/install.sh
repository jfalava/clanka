#!/bin/sh
# Builds the clanka extension for Hermes's Python and installs the plugin into
# $HERMES_HOME/plugins/clanka (default ~/.hermes), along with the clanka CLI
# that `clanka hermes profile` uses to set up a companion's profile, and the
# soul-writer skill into ~/.agents/skills so any agent on this machine
# (Hermes, Amp, Claude Code…) can write a soul the same way `clanka soul
# guide` describes.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
home=${HERMES_HOME:-$HOME/.hermes}
# `hermes -p <profile>` exports HERMES_HOME as that profile's directory, so an
# agent running this from inside Hermes would look under the profile; the
# shared plugins and profiles live in the root above it.
case $home in */profiles/*) home=${home%/profiles/*} ;; esac
python=${PYO3_PYTHON:-$(ls -d "$HOME"/.hermes/tools/python-3.*/bin/python3 | sort -V | tail -1)}
PYO3_PYTHON=$python cargo build --release -p clanka-py -p clanka --manifest-path "$root/Cargo.toml"
dest=$home/plugins/clanka
mkdir -p "$dest"
cp "$root/hermes/clanka/plugin.yaml" "$root/hermes/clanka/__init__.py" "$dest/"
# Never overwrite the extension in place: a running gateway has it mapped into
# memory, and rewriting that file crashes the gateway on its next call into it.
# A new file renamed over the old one leaves the running copy intact.
cp "$root/target/release/lib_clanka.so" "$dest/.clanka.so.new"
mv -f "$dest/.clanka.so.new" "$dest/_clanka.abi3.so"
skills=${AGENT_SKILLS_HOME:-$HOME/.agents/skills}/clanka-soul-writer
mkdir -p "$skills"
cp "$root/skills/soul-writer/SKILL.md" "$skills/SKILL.md"
echo "Installed to $dest; the soul-writer skill to $skills"
echo "Set up a companion's profile: $root/target/release/clanka hermes profile <profile> <soul.md>"
echo "A gateway that already loaded clanka keeps running the old build until it restarts:"
echo "  hermes gateway restart    (or systemctl --user restart hermes-gateway.service)"
