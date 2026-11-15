#!/bin/sh
# Installs clanka's latest GitHub release: the clanka CLI into
# $CLANKA_BIN_DIR (default ~/.local/bin), the Hermes plugin into
# $HERMES_HOME/plugins/clanka (default ~/.hermes), and the soul-writer skill
# into ~/.agents/skills so any agent on this machine (Hermes, Amp, Claude
# Code…) can write a soul the same way `clanka soul guide` describes. The
# release archive is checked against the release's SHA256SUMS before anything
# is installed. Releases are for Linux on x86_64 or arm64 and macOS on Apple
# Silicon. Then `clanka hermes refresh` gives every profile clanka made the new
# plugin and restarts the gateway; arguments go to it, so
# `hermes/install.sh --no-restart` leaves the gateway running.
#
# Without a clone: curl -fsSL https://clanka.jfa.dev/install.sh | sh
# (the docs site serves this file as it is: docs/src/pages/install.sh.ts)
#
# From a clone, `hermes/install.sh --source` builds and installs the checkout
# instead, to try a change before it is released.
set -eu
home=${HERMES_HOME:-$HOME/.hermes}
# `hermes -p <profile>` exports HERMES_HOME as that profile's directory, so an
# agent running this from inside Hermes would look under the profile; the
# shared plugins and profiles live in the root above it.
case $home in */profiles/*) home=${home%/profiles/*} ;; esac
bin=${CLANKA_BIN_DIR:-$HOME/.local/bin}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# What gets installed, laid out as in a release archive.
files=$tmp/release
mkdir "$files"

if [ "${1-}" = --source ]; then
  shift
  root=$(cd "$(dirname "$0")/.." && pwd)
  python=${PYO3_PYTHON:-$(ls -d "$HOME"/.hermes/tools/python-3.*/bin/python3 | sort -V | tail -1)}
  PYO3_PYTHON=$python cargo build --release -p clanka-py -p clanka --manifest-path "$root/Cargo.toml"
  case $(uname -s) in Darwin) lib=lib_clanka.dylib ;; *) lib=lib_clanka.so ;; esac
  mkdir -p "$files/hermes/clanka" "$files/skills/soul-writer"
  cp "$root/target/release/clanka" "$files/"
  cp "$root/target/release/$lib" "$files/hermes/clanka/_clanka.abi3.so"
  cp "$root/hermes/clanka/plugin.yaml" "$root/hermes/clanka/__init__.py" "$files/hermes/clanka/"
  cp "$root/skills/soul-writer/SKILL.md" "$files/skills/soul-writer/"
else
  case $(uname -s)-$(uname -m) in
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    *)
      echo "There is no clanka release for $(uname -s) on $(uname -m); build it from source." >&2
      exit 1
      ;;
  esac
  archive=clanka-$target.tar.gz
  url=https://github.com/jfalava/clanka/releases/latest/download
  echo "Downloading $url/$archive"
  curl -fsSL -o "$tmp/$archive" "$url/$archive"
  curl -fsSL -o "$tmp/SHA256SUMS" "$url/SHA256SUMS"
  expected=$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
  if command -v sha256sum >/dev/null; then
    found=$(sha256sum "$tmp/$archive" | cut -d ' ' -f 1)
  else
    found=$(shasum -a 256 "$tmp/$archive" | cut -d ' ' -f 1)
  fi
  if [ -z "$expected" ] || [ "$found" != "$expected" ]; then
    echo "$archive does not match the release's SHA256SUMS (${expected:-no entry}); nothing was installed." >&2
    exit 1
  fi
  tar -xzf "$tmp/$archive" -C "$files"
fi

# Never overwrite the binary or the extension in place: whatever runs it has
# it mapped into memory, and rewriting that file crashes it (on macOS it also
# fails its code signature). A new file renamed over the old one leaves the
# running copy intact.
mkdir -p "$bin"
cp "$files/clanka" "$bin/.clanka.new"
mv -f "$bin/.clanka.new" "$bin/clanka"
dest=$home/plugins/clanka
mkdir -p "$dest"
cp "$files/hermes/clanka/plugin.yaml" "$files/hermes/clanka/__init__.py" "$dest/"
cp "$files/hermes/clanka/_clanka.abi3.so" "$dest/.clanka.so.new"
mv -f "$dest/.clanka.so.new" "$dest/_clanka.abi3.so"
skills=${AGENT_SKILLS_HOME:-$HOME/.agents/skills}/clanka-soul-writer
mkdir -p "$skills"
cp "$files/skills/soul-writer/SKILL.md" "$skills/SKILL.md"
echo "Installed $("$bin/clanka" --version) to $bin, the plugin to $dest, the soul-writer skill to $skills"
case :$PATH: in *:"$bin":*) ;; *) echo "$bin is not on your PATH; add it to run clanka." ;; esac
echo "Set up a companion's profile: clanka hermes profile <profile> <soul.md>"
HERMES_HOME=$home "$bin/clanka" hermes refresh "$@"
