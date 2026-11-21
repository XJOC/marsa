#!/usr/bin/env bash
# End to end: Valkey 8.1 as a service of marsad, with a real server (ADR
# 0023). On a GitHub runner.
#
# Valkey is not in the signed manifest yet, and its release is a draft until
# the owner publishes it, so the archive comes from VALKEY_ARCHIVE: a local
# path, or an https URL. `{platform}` in it becomes this system's platform
# (darwin-arm64, darwin-x64, linux-x64, linux-arm64). A URL is checked
# against the `sha256sums.txt` beside it, as `cargo xtask manifest build`
# checks it. The archive is unpacked where `marsa install valkey` would put
# it. Every marsa command runs with --data-dir in a new folder; no sudo.
#
# Before it: `cargo build --release -p marsa`.
set -euo pipefail

if [ "${CI:-}" != "true" ]; then
  echo "This script downloads Valkey and PHP. It runs only with CI=true (a runner)." >&2
  exit 2
fi
if [ -z "${VALKEY_ARCHIVE:-}" ]; then
  echo "Set VALKEY_ARCHIVE to a Valkey archive of .github/workflows/valkey-build.yml: a local" >&2
  echo "path, or an https URL, where {platform} is replaced, for example" >&2
  echo "https://github.com/XJOC/marsa/releases/download/valkey-8.1.11-r1/valkey-8.1.11-{platform}.tar.gz" >&2
  echo "Nothing was built or started." >&2
  exit 2
fi

REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$REPO/target/release
VERSION=${VALKEY_VERSION:-8.1.11}
RUNNER_TEMP=${RUNNER_TEMP:?the runner sets RUNNER_TEMP}
DATA=$RUNNER_TEMP/vk-e2e
# Another port only for a run on a machine whose 6379 is taken.
PORT=${VALKEY_E2E_PORT:-6379}
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) PLATFORM=darwin-arm64 ;;
  Darwin-x86_64) PLATFORM=darwin-x64 ;;
  Linux-x86_64) PLATFORM=linux-x64 ;;
  Linux-aarch64) PLATFORM=linux-arm64 ;;
  *) echo "no Valkey build in this script for $(uname -sm)" >&2; exit 2 ;;
esac
ARCHIVE=${VALKEY_ARCHIVE//\{platform\}/$PLATFORM}
VK=$DATA/runtimes/valkey/$VERSION

step() { printf '\n\n===== %s =====\n' "$*"; }
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
run() {
  printf '$ %s\n' "$*"
  "$@"
}
marsa() { "$BIN/marsa" --data-dir "$DATA" "$@"; }
cli() { "$VK/bin/valkey-cli" -h 127.0.0.1 -p "$PORT" "$@"; }
# Fails unless `cmd` fails and its output has `text`.
refused() {
  local text=$1
  shift
  local out
  if out=$("$@" 2>&1); then
    fail "accepted, but must be refused: $* -> $out"
  fi
  printf '%s\n' "$out"
  grep -qF -- "$text" <<<"$out" || fail "refused, but without \"$text\": $*"
}
# Fails unless Valkey answers the command with an error that has `text`.
# valkey-cli may exit 0 on an error reply, so its output decides.
denied() {
  local text=$1
  shift
  local out
  out=$(cli "$@" 2>&1 || true)
  printf '%s -> %s\n' "$*" "$out"
  grep -qiF -- "$text" <<<"$out" || fail "not refused with \"$text\": $*"
}
# The pid of our valkey-server, from the PID column of `marsa services`.
valkey_pid() { marsa services | awk '$1 == "valkey" { print $3 }'; }
# Every TCP listener of our valkey-server.
listeners() {
  local pid
  pid=$(valkey_pid)
  if [ "$(uname -s)" = Linux ]; then
    ss -Hltnp | grep -F "pid=$pid," | awk '{print $4}'
  else
    lsof -nP -a -p "$pid" -iTCP -sTCP:LISTEN -Fn | sed -n 's/^n//p'
  fi
}
rss_mb() { echo $(($(ps -o rss= -p "$1") / 1024)); }
mode() { ls -ld "$1" | cut -c1-10; }

# What helps to understand a failure. Never `run/daemon.json`: it has the token.
diagnose() {
  step "diagnosis after a failure"
  set +e
  marsa services
  marsa logs valkey | tail -n 60
  tail -n 60 "$DATA/logs/marsad.log"
  listeners
}
trap 'code=$?; [ "$code" -eq 0 ] || diagnose; marsa daemon stop >/dev/null 2>&1; exit "$code"' EXIT

step "Valkey $VERSION for $PLATFORM from $ARCHIVE"
rm -rf "$DATA"
mkdir -p "$VK" "$DATA/config"
FILE=$RUNNER_TEMP/$(basename "$ARCHIVE")
case "$ARCHIVE" in
  https://*)
    run curl -fsSL -o "$FILE" "$ARCHIVE"
    run curl -fsSL -o "$RUNNER_TEMP/sha256sums.txt" "$(dirname "$ARCHIVE")/sha256sums.txt"
    want=$(awk -v name="$(basename "$ARCHIVE")" '$2 == name || $2 == "*" name { print $1 }' "$RUNNER_TEMP/sha256sums.txt")
    got=$(shasum -a 256 "$FILE" | awk '{print $1}')
    [ -n "$want" ] || fail "sha256sums.txt does not list $(basename "$ARCHIVE")"
    [ "$want" = "$got" ] || fail "sha256 $got, expected $want"
    ;;
  /*) [ -f "$ARCHIVE" ] || fail "no file at $ARCHIVE"; cp "$ARCHIVE" "$FILE" ;;
  *) fail "VALKEY_ARCHIVE must be an https URL or an absolute path" ;;
esac
# No top folder: bin/, licenses/, and BUILD-INFO.txt at the root.
tar -xzf "$FILE" -C "$VK"
for path in bin/valkey-server bin/valkey-cli BUILD-INFO.txt licenses; do
  [ -e "$VK/$path" ] || fail "the archive has no $path at its root"
done
[ ! -e "$VK/bin/redis-cli" ] || fail "the archive ships redis-cli"
"$VK/bin/valkey-server" --version | tee "$RUNNER_TEMP/version.txt"
grep -q "^Valkey server v=$VERSION " "$RUNNER_TEMP/version.txt" || fail "valkey-server --version"
printf '[valkey]\ndefault = "8.1"\nport = %s\n' "$PORT" >"$DATA/config/global.toml"

step "marsa services start valkey"
begun=$(date +%s)
run marsa services start valkey
echo "the start took $(($(date +%s) - begun)) s"
marsa services | tee "$RUNNER_TEMP/services.txt"
grep -E '^valkey +running' "$RUNNER_TEMP/services.txt" || fail "valkey is not running"
grep -qF "valkey: 127.0.0.1:$PORT, no password" "$RUNNER_TEMP/services.txt" ||
  fail "no connection line"
grep -qx "REDIS_PORT=$PORT" "$RUNNER_TEMP/services.txt" || fail "no REDIS_PORT line"
grep -q 'enabled = true' "$DATA/config/global.toml" || fail "the start did not turn it on"
[ "$(cli ping)" = PONG ] || fail "PING"
[ "$(mode "$DATA/run")" = drwx------ ] || fail "run/ is not 0700"
[ "$(mode "$DATA/run/valkey.conf")" = -rw------- ] || fail "valkey.conf is not 0600"
[ "$(mode "$DATA/databases/valkey")" = drwx------ ] || fail "databases/valkey is not 0700"
[ -s "$DATA/run/valkey.pid" ] || fail "no pid record"

step "it listens on 127.0.0.1:$PORT only"
[ -n "$(listeners)" ] || fail "no listener"
listeners | tee "$RUNNER_TEMP/listeners.txt"
[ "$(cat "$RUNNER_TEMP/listeners.txt")" = "127.0.0.1:$PORT" ] || fail "listeners"

step "memory when idle: under 10 MB"
sleep 5
PID=$(valkey_pid)
echo "valkey-server rss: $(rss_mb "$PID") MB"
[ "$(rss_mb "$PID")" -lt 10 ] || fail "too much memory"

step "access (ADR 0023): no file writes, no DEBUG, no modules over TCP"
# No CONFIG SET at all (an ACL rule): `bind` and `protected-mode` are not
# protected configs, so they would let a local user open the server to the
# network.
denied 'NOPERM' config set dir "$RUNNER_TEMP"
denied 'NOPERM' config set dbfilename pwn.php
denied 'NOPERM' config set bind 0.0.0.0
denied 'NOPERM' config set protected-mode no
denied 'DEBUG command not allowed' debug sleep 0
denied 'MODULE command not allowed' module load "$RUNNER_TEMP/pwn.so"
[ "$(cli config get dir | tail -n 1)" = "$DATA/databases/valkey" ] || fail "dir changed"
[ "$(cli config get save | tail -n 1)" = "" ] || fail "background saves are on"
[ "$(cli config get appendonly | tail -n 1)" = no ] || fail "the AOF is on"
[ "$(cli config get bind | tail -n 1)" = 127.0.0.1 ] || fail "bind"

step "the commands Laravel's cache and queue use"
[ "$(cli set marsa-e2e kept)" = OK ] || fail "SET"
[ "$(cli get marsa-e2e)" = kept ] || fail "GET"
[ "$(cli eval "return redis.call('GET', KEYS[1])" 1 marsa-e2e)" = kept ] || fail "EVAL"
[ "$(cli rpush marsa-e2e-queue job)" = 1 ] || fail "RPUSH"

step "phpredis (the redis extension of our PHP)"
run marsa install php 8.4
PHP=$(ls -d "$DATA"/runtimes/php/8.4.*/bin/php | tail -n 1)
"$PHP" -r '
$r = new Redis();
$r->connect("127.0.0.1", '"$PORT"');
$r->set("marsa-e2e-php", "phpredis");
echo $r->get("marsa-e2e-php"), "\n";
' | grep -qx phpredis || fail "phpredis"

# TODO (Laravel): a new Laravel project with CACHE_STORE=redis and
# QUEUE_CONNECTION=redis, `php artisan` cache and queue commands once with
# phpredis (REDIS_CLIENT=phpredis, the default) and once with predis
# (composer require predis/predis, REDIS_CLIENT=predis), then a stop and a
# start, and the cached value is still there. Needs Composer and Packagist on
# the runner (the spike behind ADR 0023 had a 39-check suite for predis).

step "the running version cannot be uninstalled"
refused 'in use' marsa uninstall valkey "$VERSION"
[ -d "$VK" ] || fail "the running version was removed"

step "a stop sends SHUTDOWN SAVE: dump.rdb is written, in under 5 s"
begun=$(date +%s)
run marsa services stop valkey
took=$(($(date +%s) - begun))
echo "the stop took $took s"
[ "$took" -lt 5 ] || fail "the stop took $took s"
[ -s "$DATA/databases/valkey/dump.rdb" ] || fail "no dump.rdb"
grep -q 'DB saved on disk' "$DATA/logs/valkey.log" || fail "no 'DB saved on disk' in the log"
grep -q 'still running after SHUTDOWN SAVE' "$DATA/logs/marsad.log" && fail "SHUTDOWN SAVE did not stop it"
grep -q 'enabled = true' "$DATA/config/global.toml" && fail "the stop did not turn it off"
[ -e "$DATA/run/valkey.pid" ] && fail "the pid record stayed"

step "the next start finds the data"
run marsa services start valkey
[ "$(cli get marsa-e2e)" = kept ] || fail "the data did not survive the stop"
run marsa services stop valkey

step "another program holds the port: refused, with its name"
python3 -c "import socket, time; s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', $PORT)); s.listen(); time.sleep(300)" &
HOLDER=$!
sleep 1
refused 'service_port_busy' marsa services start valkey
kill "$HOLDER"
wait "$HOLDER" || true

step "it starts with marsad when on"
run marsa services start valkey
run marsa daemon stop
run marsa daemon start
started=false
for _ in $(seq 1 30); do
  if marsa services | grep -qE '^valkey +running' && [ "$(cli ping 2>/dev/null)" = PONG ]; then
    started=true
    break
  fi
  sleep 1
done
[ "$started" = true ] || fail "valkey did not start with marsad"
[ "$(cli get marsa-e2e)" = kept ] || fail "the data did not survive"

step "marsad killed: the next start stops our old valkey-server with SIGTERM, which saves"
OLD=$(valkey_pid)
[ "$(cli set marsa-e2e-orphan saved)" = OK ] || fail "SET"
echo "--- run/ before the kill"
ls -la "$DATA/run"
tail -n 12 "$DATA/logs/marsad.log"
pkill -9 -f "marsad.*--data-dir $DATA"
sleep 1
kill -0 "$OLD" || fail "valkey-server died with marsad"
run marsa services start valkey
kill -0 "$OLD" 2>/dev/null && fail "the orphan still runs"
marsa logs valkey | grep -F "that an earlier marsad left running; it ended after SIGTERM" ||
  fail "no line about the orphan"
[ "$(cli get marsa-e2e-orphan)" = saved ] || fail "SIGTERM did not save the data"
run marsa services stop valkey

step "PASS"
