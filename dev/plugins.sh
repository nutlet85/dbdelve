#!/usr/bin/env bash
# Builds the plugin crates and stages them where `db::plugin::discover()` looks
# under `cargo run`: target/<profile>/plugins/<id>/{dbdelve-plugin.toml,bin/<exe>}.
# Usage: dev/plugins.sh [--release|--profile <name>] [--echo]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

profile=debug
cargo_flags=()
plugins=(odbc)
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --release) profile=release; cargo_flags=(--release); shift ;;
    --profile)
      [[ "$#" -ge 2 ]] || {
        echo "dev/plugins.sh: --profile needs a name" >&2
        exit 2
      }
      profile="$2"
      cargo_flags=(--profile "$2")
      shift 2
      ;;
    # Test code, never shipped: the fake engine the host's tests drive.
    --echo) plugins+=(echo); shift ;;
    *) echo "usage: dev/plugins.sh [--release|--profile <name>] [--echo]" >&2; exit 2 ;;
  esac
done

target_dir="${CARGO_TARGET_DIR:-$ROOT/target}"
stage="$target_dir/$profile/plugins"
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) suffix=.exe ;; *) suffix= ;; esac

# Before the build: overwriting the executable of a running bridge breaks it.
if pgrep -f "$stage/" >/dev/null; then
  echo "a plugin from $stage is running -- quit the app and re-run" >&2
  exit 1
fi

packages=()
for id in "${plugins[@]}"; do
  packages+=(-p "dbdelve-plugin-$id")
done

if ! cargo build ${cargo_flags[@]+"${cargo_flags[@]}"} "${packages[@]}"; then
  echo "if the link failed on -lodbc: install unixODBC's development files (apt install unixodbc-dev)" >&2
  exit 1
fi

for id in "${plugins[@]}"; do
  dest="$stage/$id"
  exe="$(sed -n 's/^executable *= *"\(.*\)"/\1/p' "plugins/$id/dbdelve-plugin.toml")"
  rm -rf "$dest"
  mkdir -p "$dest/$(dirname "$exe")"
  cp "plugins/$id/dbdelve-plugin.toml" "$dest/"
  cp "$target_dir/$profile/$(basename "$exe")$suffix" "$dest/$exe$suffix"
  echo "staged $dest"
done
