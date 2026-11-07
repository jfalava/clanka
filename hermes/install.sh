#!/bin/sh
# Builds the clanka extension for Hermes's Python and installs the plugin into
# $HERMES_HOME/plugins/clanka (default ~/.hermes). Enable it afterwards with
# `hermes plugins enable clanka` and set CLANKA_SOUL.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
home=${HERMES_HOME:-$HOME/.hermes}
python=${PYO3_PYTHON:-$(ls -d "$HOME"/.hermes/tools/python-3.*/bin/python3 | sort -V | tail -1)}
PYO3_PYTHON=$python cargo build --release -p clanka-py --manifest-path "$root/Cargo.toml"
dest=$home/plugins/clanka
mkdir -p "$dest"
cp "$root/hermes/clanka/plugin.yaml" "$root/hermes/clanka/__init__.py" "$dest/"
cp "$root/target/release/lib_clanka.so" "$dest/_clanka.abi3.so"
echo "Installed to $dest"
