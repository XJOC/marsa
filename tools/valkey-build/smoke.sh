#!/usr/bin/env bash
# Checks an unpacked Valkey package the way Marsa will use it. Blocks the
# release if anything is wrong.
#
#   VALKEY_VERSION=8.1.11 smoke.sh <package dir> <platform>
#
# The package dir is the archive as shipped, unpacked (bin/, licenses/,
# BUILD-INFO.txt). This script never starts anything except the package's own
# valkey-server, on 127.0.0.1, with a random port and a throw-away data dir.
#
# 1. The files are there, and the linking is what we promise:
#    macOS: only libSystem, minos 12.0. Linux: statically linked.
#    Windows: the two Cygwin DLLs sit next to the programs.
# 2. valkey-server --version says the version we built.
# 3. PING, SET/GET, EVAL.
# 4. SHUTDOWN SAVE writes dump.rdb, and a new start finds the value.
#
# On Windows run it from Git Bash with no Cygwin in PATH, so the programs must
# find their DLLs in bin/.
set -euo pipefail
export LC_ALL=C

: "${VALKEY_VERSION:?VALKEY_VERSION is required}"
PKG_ARG="${1:?usage: smoke.sh <package dir> <platform>}"
PLATFORM="${2:?usage: smoke.sh <package dir> <platform>}"
PKG="$(cd "$PKG_ARG" && pwd)"

fail() { echo "SMOKE FAILED: $*" >&2; exit 1; }
say() { echo "--- $*"; }

# --- 1. files and linking
exe=""
[ "$PLATFORM" = "windows-x64" ] && exe=".exe"
SERVER="$PKG/bin/valkey-server$exe"
CLI="$PKG/bin/valkey-cli$exe"
for f in "$SERVER" "$CLI" "$PKG/BUILD-INFO.txt" "$PKG/licenses/COPYING" "$PKG/licenses/NOTICE.txt"; do
  [ -s "$f" ] || fail "missing or empty: $f"
done
[ -x "$SERVER" ] && [ -x "$CLI" ] || fail "the programs are not executable"
grep -q 'BSD 3-Clause License' "$PKG/licenses/COPYING" || fail "licenses/COPYING is not Valkey's licence"

# The line that says what the binary links, per platform.
case "$PLATFORM" in
  darwin-arm64 | darwin-x64)
    for bin in "$SERVER" "$CLI"; do
      say "otool -L $(basename "$bin")"
      otool -L "$bin"
      # Line 1 is the file name. Every other line is a library.
      others="$(otool -L "$bin" | tail -n +2 | awk '{ print $1 }' | grep -v '^/usr/lib/libSystem.B.dylib$' || true)"
      [ -z "$others" ] || fail "$(basename "$bin") links more than libSystem: $others"
      # A binary built by the new toolchain has LC_BUILD_VERSION (minos); an old one has LC_VERSION_MIN_MACOSX (version).
      minos="$(otool -l "$bin" | awk '
        /cmd LC_BUILD_VERSION/ { f = "build"; next }
        /cmd LC_VERSION_MIN_MACOSX/ { f = "min"; next }
        f == "build" && $1 == "minos" { print $2; exit }
        f == "min" && $1 == "version" { print $2; exit }')"
      [ "$minos" = "12.0" ] || fail "$(basename "$bin") has minos '$minos', expected 12.0"
      say "$(basename "$bin"): minos $minos"
    done
    if [ "$PLATFORM" = "darwin-arm64" ]; then
      codesign --verify --strict "$SERVER" "$CLI" || fail "the arm64 binaries are not validly signed"
    fi
    ;;
  linux-x64 | linux-arm64)
    for bin in "$SERVER" "$CLI"; do
      say "file $(basename "$bin")"
      description="$(file -b "$bin")"
      echo "$description"
      case "$description" in
        *"statically linked"* | *"static-pie linked"*) ;;
        *) fail "$(basename "$bin") is not statically linked" ;;
      esac
    done
    [ -s "$PKG/licenses/musl.COPYRIGHT" ] || fail "missing licenses/musl.COPYRIGHT"
    ;;
  windows-x64)
    for f in cygwin1.dll cyggcc_s-seh-1.dll; do
      [ -s "$PKG/bin/$f" ] || fail "missing bin/$f"
    done
    for f in cygwin.CYGWIN_LICENSE cygwin.LGPL-3.0.txt gcc-libgcc.GPL-3.0.txt gcc-libgcc.GCC-Runtime-Library-Exception-3.1.txt; do
      [ -s "$PKG/licenses/$f" ] || fail "missing licenses/$f"
    done
    ;;
  *) fail "unknown platform $PLATFORM" ;;
esac

# --- 2. the version
say "version"
version_line="$("$SERVER" --version)"
echo "$version_line"
case "$version_line" in
  "Valkey server v=$VALKEY_VERSION "*) ;;
  *) fail "expected 'Valkey server v=$VALKEY_VERSION ', got: $version_line" ;;
esac

# --- 3 and 4. a real server
# A path the Windows (Cygwin) programs understand; a no-op elsewhere.
native() { if command -v cygpath > /dev/null; then cygpath -m "$1"; else printf '%s\n' "$1"; fi; }

DATA="$(mktemp -d)"
SERVER_PID=""
PORT=""
stop_leftover() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2> /dev/null; then kill "$SERVER_PID" 2> /dev/null || true; fi
  rm -rf "$DATA"
}
trap stop_leftover EXIT
NATIVE_DATA="$(native "$DATA")"

cli() { "$CLI" -h 127.0.0.1 -p "$PORT" "$@"; }

# Starts the server on a random port and waits for PING. A port that is taken
# makes the server exit at once; then the next random port is tried.
start_server() {
  local attempt i
  for attempt in 1 2 3 4 5; do
    PORT=$((20000 + (RANDOM % 30000)))
    "$SERVER" --bind 127.0.0.1 --port "$PORT" --dir "$NATIVE_DATA" --save "" --appendonly no --daemonize no \
      > "$DATA/server-$attempt.log" 2>&1 &
    SERVER_PID=$!
    for i in $(seq 1 100); do
      if [ "$(cli ping 2> /dev/null || true)" = "PONG" ]; then
        echo "server up on 127.0.0.1:$PORT (attempt $attempt)"
        return 0
      fi
      kill -0 "$SERVER_PID" 2> /dev/null || break
      sleep 0.2
    done
    echo "attempt $attempt on port $PORT failed:"
    cat "$DATA/server-$attempt.log" || true
    kill "$SERVER_PID" 2> /dev/null || true
    wait "$SERVER_PID" 2> /dev/null || true
  done
  fail "the server did not start"
}

# Waits until the server process is gone.
wait_gone() {
  local i
  for i in $(seq 1 150); do
    kill -0 "$SERVER_PID" 2> /dev/null || { wait "$SERVER_PID" 2> /dev/null || true; return 0; }
    sleep 0.2
  done
  fail "the server did not stop after SHUTDOWN"
}

expect() { # <what> <wanted> <got>
  [ "$2" = "$3" ] || fail "$1: expected '$2', got '$3'"
  echo "ok: $1 = $3"
}

say "start"
start_server
expect "PING" "PONG" "$(cli ping)"
expect "SET" "OK" "$(cli set marsa-smoke hello)"
expect "GET" "hello" "$(cli get marsa-smoke)"
expect "EVAL arithmetic" "2" "$(cli eval 'return 1 + 1' 0)"
expect "EVAL reads a key" "hello" "$(cli eval "return redis.call('GET', KEYS[1])" 1 marsa-smoke)"
expect "config bind" "127.0.0.1" "$(cli config get bind | tail -n 1)"

say "SHUTDOWN SAVE"
cli shutdown save > /dev/null 2>&1 || true
wait_gone
[ -s "$DATA/dump.rdb" ] || fail "SHUTDOWN SAVE wrote no dump.rdb in $DATA"
echo "dump.rdb: $(wc -c < "$DATA/dump.rdb") bytes"

say "start again on the same data"
start_server
expect "GET after restart" "hello" "$(cli get marsa-smoke)"
cli shutdown nosave > /dev/null 2>&1 || true
wait_gone

echo "SMOKE OK: $PLATFORM $VALKEY_VERSION"
