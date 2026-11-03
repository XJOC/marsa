#!/usr/bin/env bash
# S-03 path A: can Xdebug be linked into the PHP binary itself (static)?
# Asks spc directly (no download, no build; it refuses in about 1 second) and
# prints spc's own package config for ext-xdebug.
# Run:  ./12-path-a.sh      Output: results/path-a.txt
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
SPC="$DL/spc-macos-aarch64"
mkdir -p "$WORK/spc-run" && cd "$WORK/spc-run"   # spc writes caches in the current dir
{
  echo "== $("$SPC" --version 2>&1 | head -1)"
  echo; echo "== spc dev:info ext-xdebug (package config, first 14 lines)"
  "$SPC" dev:info ext-xdebug 2>&1 | head -14
  echo; echo "== spc build:php xdebug --build-cli -n --no-download"
  perl -e 'alarm 120; exec @ARGV' "$SPC" build:php xdebug --build-cli -n --no-download 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -v -E '^\s*$|____|\\_|/ _|\| |_\|' || true
} > "$RESULTS/path-a.txt"
cat "$RESULTS/path-a.txt"
