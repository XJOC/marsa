#!/usr/bin/env bash
# `marsa install` from the signed runtimes manifest to programs that run, on
# macOS, Linux, and Windows (Git Bash). Task M2-08, the goal of milestone 2.
#
# It downloads PHP, Node, and Composer from the real internet into Marsa's
# default data folder, so it runs only on a GitHub runner: it refuses to
# start anywhere else. It changes no system file and needs no sudo. It is
# not part of `cargo nextest`.
#
# Before it: `cargo build --release -p marsa`, then
# `cargo build --release -p marsa-shim` (the shim is always built alone).
set -euo pipefail

if [ "${CI:-}" != "true" ]; then
  echo "This script downloads runtimes into Marsa's data folder. It runs only with CI=true (a runner)." >&2
  exit 2
fi

REPO=$(cd "$(dirname "$0")/../.." && pwd)
# `config::app_data_dir()`: `directories::ProjectDirs::from("dev", "marsa",
# "Marsa").data_local_dir()` on each system.
case "$(uname -s)" in
  Darwin)
    OS=macos
    DATA="$HOME/Library/Application Support/dev.marsa.Marsa"
    ;;
  Linux)
    OS=linux
    DATA="${XDG_DATA_HOME:-$HOME/.local/share}/marsa"
    ;;
  MINGW* | MSYS*)
    OS=windows
    DATA="$(cygpath -u "$LOCALAPPDATA")/marsa/Marsa/data"
    ;;
  *)
    echo "unknown system: $(uname -s)" >&2
    exit 2
    ;;
esac
EXE=""
[ "$OS" = windows ] && EXE=".exe"
BIN="$DATA/bin"
SHIMS="$DATA/shims"
RUNTIMES="$DATA/runtimes"

step() { printf '\n\n===== %s =====\n' "$*"; }
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
# Prints the command, then runs it.
run() {
  printf '$ %s\n' "$*"
  "$@"
}
# Runs marsa and keeps its output and exit code in OUT and CODE.
marsa_out() {
  printf '$ marsa %s\n' "$*"
  CODE=0
  OUT=$("$BIN/marsa$EXE" "$@" 2>&1) || CODE=$?
  printf '%s\n(exit %s)\n' "$OUT" "$CODE"
}
# The one version folder of `tool`.
version_dir() {
  local dirs
  dirs=("$RUNTIMES/$1"/*/)
  [ "${#dirs[@]}" = 1 ] && [ -d "${dirs[0]}" ] || fail "expected one folder in $RUNTIMES/$1, found: ${dirs[*]}"
  echo "${dirs[0]%/}"
}
# Logs only; never `run/daemon.json`, which holds the daemon token.
diagnose() {
  step "diagnosis after a failure"
  set +e
  ls -la "$DATA" "$DATA/logs" "$RUNTIMES"/* 2>&1
  tail -n 60 "$DATA"/logs/*.log
}
trap 'status=$?; [ "$status" = 0 ] || diagnose' EXIT

step "the programs, in <app data>/bin"
echo "system: $OS, data folder: $DATA"
mkdir -p "$BIN" "$SHIMS"
# `marsa` starts the `marsad` next to it (M3-12).
for program in marsa marsad marsa-shim; do
  cp "$REPO/target/release/$program$EXE" "$BIN/$program$EXE"
done
run "$BIN/marsa$EXE" --version

step "1. marsa install php 8.4"
marsa_out install php 8.4
[ "$CODE" = 0 ] || fail "marsa install php 8.4 exited with $CODE"

step "2. again: already installed"
marsa_out install php 8.4
[ "$CODE" = 0 ] || fail "the second install exited with $CODE"
grep -q "already installed" <<<"$OUT" || fail "the second install did not say \"already installed\""

step "3. marsa install node 22, and composer"
marsa_out install node 22
[ "$CODE" = 0 ] || fail "marsa install node 22 exited with $CODE"
marsa_out install composer
[ "$CODE" = 0 ] || fail "marsa install composer exited with $CODE"

step "4. marsa runtimes lists the three"
marsa_out runtimes
[ "$CODE" = 0 ] || fail "marsa runtimes exited with $CODE"
for tool in php node composer; do
  grep -qi "$tool" <<<"$OUT" || fail "marsa runtimes does not list $tool"
done
du -sh "$RUNTIMES" || true

step "5. the programs run"
PHP_DIR=$(version_dir php)
NODE_DIR=$(version_dir node)
COMPOSER_DIR=$(version_dir composer)
if [ "$OS" = windows ]; then
  PHP="$PHP_DIR/php.exe"
  NODE="$NODE_DIR/node.exe"
  NPM_CLI="$NODE_DIR/node_modules/npm/bin/npm-cli.js"
else
  PHP="$PHP_DIR/bin/php"
  NODE="$NODE_DIR/bin/node"
  NPM_CLI="$NODE_DIR/lib/node_modules/npm/bin/npm-cli.js"
fi
out=$(run "$PHP" -v)
echo "$out"
grep -q "^PHP 8\.4\." <<<"$out" || fail "php -v is not PHP 8.4"
if [ "$OS" != windows ]; then
  out=$(run "$PHP_DIR/bin/php-fpm" -v)
  echo "$out"
  grep -q "^PHP 8\.4\." <<<"$out" || fail "php-fpm -v is not PHP 8.4"
fi
out=$(run "$NODE" -v)
echo "$out"
grep -q "^v22\." <<<"$out" || fail "node -v is not v22"
out=$(run "$NODE" "$NPM_CLI" -v)
echo "$out"
grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+' <<<"$out" || fail "npm -v printed no version"
out=$(run "$PHP" "$COMPOSER_DIR/composer.phar" --version)
echo "$out"
grep -q "Composer version 2\." <<<"$out" || fail "composer --version is not Composer 2"

if [ "$OS" != windows ]; then
  step "6. the shim picks the version of .php-version (M1-06)"
  for name in php node composer; do
    ln -f "$BIN/marsa-shim" "$SHIMS/$name"
  done
  project=$(mktemp -d)
  echo 8.4 >"$project/.php-version"
  out=$(cd "$project" && PATH="$SHIMS:$PATH" run php -v)
  echo "$out"
  grep -q "^PHP 8\.4\." <<<"$out" || fail "the shim did not run PHP 8.4"
  out=$(cd "$project" && PATH="$SHIMS:$PATH" run node -v)
  grep -q "^v22\." <<<"$out" || fail "the shim did not run node 22"
fi

step "7. a version that does not exist"
marsa_out install php 7.0
[ "$CODE" = 1 ] || fail "marsa install php 7.0 exited with $CODE, not 1"
grep -qF "[install_no_such_version]" <<<"$OUT" || fail "no [install_no_such_version]"

run "$BIN/marsa$EXE" daemon stop
echo "PASS: marsa install on $OS"
