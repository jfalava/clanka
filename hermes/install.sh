#!/bin/sh
# Builds the clanka extension for Hermes's Python and installs the plugin into
# $HERMES_HOME/plugins/clanka (default ~/.hermes), along with the clanka CLI
# that hermes/profile.sh uses to set up a companion's profile.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
home=${HERMES_HOME:-$HOME/.hermes}
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
echo "Installed to $dest"
echo "Set up a companion's profile: $root/hermes/profile.sh <profile> <soul.md>"
echo "A gateway that already loaded clanka keeps running the old native code until it restarts."
