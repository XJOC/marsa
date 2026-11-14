#!/usr/bin/env bash
# S-12b: Valkey on macOS and Linux. Subcommands:
#
#   kv.sh obtain official|build|build-static   get a valkey-server (see below)
#   kv.sh probe                                start, stop, orphan, memory, linking
#   kv.sh laravel                              run the Laravel cache and queue probes
#   kv.sh summary                              print the facts and the Laravel results
#
# Environment: LABEL (names the cell), OUT (results dir), WORK (scratch dir),
# VALKEY_VERSION (default 8.1.11). Every server binds 127.0.0.1 and uses a data
# dir under $WORK. Runs on GitHub runners only: never run this on a real machine.
set -uo pipefail

KV="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
V="${VALKEY_VERSION:-8.1.11}"
LABEL="${LABEL:-local}"
WORK="${WORK:-${RUNNER_TEMP:-/tmp}/kv-work}"
OUT="${OUT:-${RUNNER_TEMP:-/tmp}/kv-results}"
mkdir -p "$WORK" "$OUT"
FACTS="$OUT/facts.txt"
RESP="php $KV/tools/resp.php"
OS="$(uname -s)"

say()  { echo "[$(date +%H:%M:%S)] $*"; }
fact() { printf '%s=%s\n' "$1" "$2" >>"$FACTS"; echo "FACT $1=$2"; }
now()  { php -r 'echo microtime(true);'; }
ms()   { php -r "echo (int) round(($2 - $1) * 1000);" ; }
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
ncpu() { if [ "$OS" = Darwin ]; then sysctl -n hw.ncpu; else nproc; fi; }
dir_bytes() { if [ "$OS" = Darwin ]; then find "$1" -type f -exec stat -f %z {} + | awk '{s+=$1} END {print s+0}'; else du -sb "$1" | cut -f1; fi; }
file_bytes() { if [ "$OS" = Darwin ]; then stat -f %z "$1"; else stat -c %s "$1"; fi; }

target() {
  local m; m="$(uname -m)"
  case "$OS-$m" in
    Darwin-arm64) echo darwin-arm64 ;;
    Darwin-x86_64) echo darwin-x64 ;;
    Linux-x86_64) echo linux-x64 ;;
    Linux-aarch64) echo linux-arm64 ;;
    *) echo "unknown-$OS-$m" ;;
  esac
}

# ---------------------------------------------------------------- obtain
link_report() { # BIN -> facts about the dynamic libraries
  local bin="$1"
  if [ "$OS" = Darwin ]; then
    fact otool_L "$(otool -L "$bin" | tail -n +2 | awk '{print $1}' | tr '\n' ' ')"
    fact lipo "$(lipo -info "$bin" 2>&1)"
    fact minos "$(otool -l "$bin" | grep -A4 LC_BUILD_VERSION | grep minos | awk '{print $2}' | head -1)"
    fact codesign "$(codesign -dv "$bin" 2>&1 | grep -E 'Signature|flags' | tr '\n' ' ')"
    fact xattr "$(xattr -l "$bin" | tr '\n' ' ')"
  else
    fact file "$(file -b "$bin" | cut -c1-200)"
    fact ldd "$(ldd "$bin" 2>&1 | tr '\n' ';' | tr -s '\t ' ' ')"
    fact glibc_max "$(objdump -T "$bin" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1)"
  fi
}

pack() { # SERVER CLI -> archive sizes, written as facts (server + cli only, as Marsa would ship)
  local server="$1" cli="$2" t pk
  t="$(target)"
  pk="$WORK/pack/valkey-$V-$t"
  rm -rf "$WORK/pack"; mkdir -p "$pk/bin"
  cp "$server" "$pk/bin/valkey-server"; cp "$cli" "$pk/bin/valkey-cli"
  fact size_server_unstripped_bytes "$(file_bytes "$pk/bin/valkey-server")"
  strip "$pk/bin/valkey-server" "$pk/bin/valkey-cli" 2>/dev/null || strip -x "$pk/bin/valkey-server" "$pk/bin/valkey-cli" 2>/dev/null || true
  fact size_server_stripped_bytes "$(file_bytes "$pk/bin/valkey-server")"
  fact size_cli_stripped_bytes "$(file_bytes "$pk/bin/valkey-cli")"
  (cd "$WORK/pack" && tar czf "$OUT/valkey-$V-$t.tar.gz" "valkey-$V-$t")
  fact size_archive_server_cli_bytes "$(file_bytes "$OUT/valkey-$V-$t.tar.gz")"
  fact size_unpacked_server_cli_bytes "$(dir_bytes "$pk")"
  fact sha256_of_our_archive "$(sha256 "$OUT/valkey-$V-$t.tar.gz")"
  # The stripped copy is what we measure from now on.
  echo "KV_SERVER=$pk/bin/valkey-server" >"$OUT/paths.sh"
  echo "KV_CLI=$pk/bin/valkey-cli" >>"$OUT/paths.sh"
}

obtain_official() {
  [ "$OS" = Linux ] || { fact source "none: valkey.io publishes Linux tarballs only"; return 1; }
  local a base codename other
  case "$(uname -m)" in x86_64) a=x86_64 ;; aarch64) a=arm64 ;; esac
  # shellcheck disable=SC1091
  codename="$(. /etc/os-release; echo "$VERSION_CODENAME")"
  other=$([ "$codename" = noble ] && echo jammy || echo noble)
  mkdir -p "$WORK/dl"; cd "$WORK/dl" || return 1
  for c in "$codename" "$other"; do
    base="valkey-$V-$c-$a"
    fact "url_$c" "https://download.valkey.io/releases/$base.tar.gz"
    local t0 t1; t0="$(now)"
    curl -fsSL -o "$base.tar.gz" "https://download.valkey.io/releases/$base.tar.gz" || { fact "download_$c" FAILED; continue; }
    t1="$(now)"; fact "download_ms_$c" "$(ms "$t0" "$t1")"
    curl -fsSL -o "$base.tar.gz.sha256" "https://download.valkey.io/releases/$base.tar.gz.sha256" || fact "sha256_file_$c" MISSING
    fact "sha256_published_$c" "$(cut -d' ' -f1 "$base.tar.gz.sha256")"
    if [ "$(sha256 "$base.tar.gz")" = "$(cut -d' ' -f1 "$base.tar.gz.sha256")" ]; then fact "sha256_match_$c" yes; else fact "sha256_match_$c" NO; fi
    fact "archive_bytes_$c" "$(file_bytes "$base.tar.gz")"
    mkdir -p "x-$c" && tar xzf "$base.tar.gz" -C "x-$c"
    fact "unpacked_bytes_$c" "$(dir_bytes "x-$c")"
    fact "layout_$c" "$(cd "x-$c" && find . -type f -o -type l | head -30 | tr '\n' ' ')"
    local srv; srv="$(find "x-$c" -name valkey-server \( -type f -o -type l \) | head -1)"
    fact "runs_on_this_host_$c" "$("$PWD/$srv" --version 2>&1 | head -1)"
    link_report "$PWD/$srv"
  done
  local srv cli
  srv="$(find "$WORK/dl/x-$codename" -name valkey-server | head -1)"
  cli="$(find "$WORK/dl/x-$codename" -name valkey-cli | head -1)"
  fact archive_bytes "$(file_bytes "$WORK/dl/valkey-$V-$codename-$a.tar.gz")"
  fact unpacked_bytes "$(dir_bytes "$WORK/dl/x-$codename")"
  fact license_file "$(find "$WORK/dl/x-$codename" -iname 'COPYING*' -o -iname 'LICENSE*' | head -3 | tr '\n' ' ')"
  echo "KV_SERVER=$srv" >"$OUT/paths.sh"; echo "KV_CLI=$cli" >>"$OUT/paths.sh"
  fact source "official: https://download.valkey.io/releases/valkey-$V-$codename-$a.tar.gz"
}

fetch_source() {
  mkdir -p "$WORK/src"; cd "$WORK/src" || return 1
  curl -fsSL -o "valkey-$V.tar.gz" "https://github.com/valkey-io/valkey/archive/refs/tags/$V.tar.gz" || return 1
  local want
  want="$(curl -fsSL https://raw.githubusercontent.com/valkey-io/valkey-hashes/main/README | awk -v f="valkey-$V.tar.gz" '$1=="hash" && $2==f {print $4}')"
  fact source_tarball "https://github.com/valkey-io/valkey/archive/refs/tags/$V.tar.gz"
  fact sha256_source_published_valkey_hashes "$want"
  if [ -n "$want" ] && [ "$(sha256 "valkey-$V.tar.gz")" = "$want" ]; then fact sha256_source_match yes; else fact sha256_source_match "NO (got $(sha256 "valkey-$V.tar.gz"))"; fi
  tar xzf "valkey-$V.tar.gz"
}

obtain_build() {
  fetch_source || { fact source "FAILED to fetch source"; return 1; }
  cd "$WORK/src/valkey-$V" || return 1
  local t0 t1 rc
  if [ "$OS" = Darwin ]; then export MACOSX_DEPLOYMENT_TARGET=12.0; fact deployment_target 12.0; fi
  fact cc "$(${CC:-cc} --version | head -1)"
  fact cpus "$(ncpu)"
  t0="$(now)"
  make -j"$(ncpu)" BUILD_TLS=no >"$OUT/build.log" 2>&1; rc=$?
  t1="$(now)"
  fact build_seconds "$(( $(ms "$t0" "$t1") / 1000 ))"
  fact build_rc "$rc"
  [ $rc -eq 0 ] || { tail -30 "$OUT/build.log"; fact source "own build FAILED"; return 1; }
  fact source "own build: make BUILD_TLS=no (default allocator: $([ "$OS" = Linux ] && echo jemalloc || echo libc))"
  link_report "$PWD/src/valkey-server"
  pack "$PWD/src/valkey-server" "$PWD/src/valkey-cli"
}

obtain_build_static() {
  [ "$OS" = Linux ] || return 1
  fetch_source || return 1
  cd "$WORK/src/valkey-$V" || return 1
  local t0 t1 rc
  t0="$(now)"
  docker run --rm -v "$PWD:/w" -w /w alpine:3.22 sh -c \
    'apk add --no-cache build-base linux-headers >/dev/null && make -j"$(nproc)" BUILD_TLS=no MALLOC=libc CFLAGS=-static LDFLAGS=-static' \
    >"$OUT/build.log" 2>&1; rc=$?
  t1="$(now)"
  sudo chown -R "$(id -u):$(id -g)" . 2>/dev/null || true
  fact build_seconds "$(( $(ms "$t0" "$t1") / 1000 ))"
  fact build_rc "$rc"
  [ $rc -eq 0 ] || { tail -30 "$OUT/build.log"; fact source "static musl build FAILED"; return 1; }
  fact source "own static build in alpine:3.22 (musl): make BUILD_TLS=no MALLOC=libc LDFLAGS=-static"
  link_report "$PWD/src/valkey-server"
  pack "$PWD/src/valkey-server" "$PWD/src/valkey-cli"
}

# ---------------------------------------------------------------- probe
SERVER=""; CLI=""
load_paths() {
  if [ ! -f "$OUT/paths.sh" ]; then fact binary "none: the obtain step produced no binary"; return 1; fi
  # shellcheck disable=SC1091
  . "$OUT/paths.sh"; SERVER="$KV_SERVER"; CLI="$KV_CLI"; }

PID=0
start_bg() { # PORT DIR [server args...] -> sets PID
  local port="$1" dir="$2"; shift 2
  mkdir -p "$dir"
  "$SERVER" --bind 127.0.0.1 --port "$port" --dir "$dir" --pidfile "$dir/valkey.pid" \
    --logfile "$dir/server.log" "$@" >"$dir/stdout.log" 2>&1 &
  PID=$!
}
port_open() { $RESP --port "$1" PING >/dev/null 2>&1; }
wait_exit() { # PID TIMEOUT_S -> 0 when gone; also sets EXITRC
  local pid="$1" limit="$2" i=0
  while kill -0 "$pid" 2>/dev/null; do
    sleep 0.1; i=$((i + 1)); [ $i -ge $((limit * 10)) ] && return 1
  done
  wait "$pid" 2>/dev/null; EXITRC=$?
  return 0
}
rss_kb() { ps -o rss= -p "$1" | tr -d ' '; }
info_field() { # PORT SECTION FIELD
  $RESP --port "$1" INFO "$2" | tr -d '\r' | awk -F: -v f="$3" '$1==f {print $2}'
}

probe() {
  load_paths || return
  local D="$WORK/probe" t0 t1
  rm -rf "$D"; mkdir -p "$D"
  fact binary "$SERVER"
  fact version "$("$SERVER" --version 2>&1 | head -1)"
  if [ "$OS" = Darwin ]; then
    # A copy with the quarantine attribute a browser would add: does a shell still run it?
    cp "$SERVER" "$D/quarantined-server"
    xattr -w com.apple.quarantine "0083;5f3b2a1c;Safari;" "$D/quarantined-server"
    fact quarantined_copy_runs "$("$D/quarantined-server" --version 2>&1 | head -1) (rc=$?)"
  fi

  # --- 1. default port, nothing saved, bound to loopback
  say "probe 1: port 6379, --save '' --appendonly no"
  t0="$(now)"
  start_bg 6379 "$D/p6379" --save "" --appendonly no
  if $RESP --port 6379 --wait 20 PING >/dev/null 2>&1; then
    t1="$(now)"; fact start_to_ping_ms "$(ms "$t0" "$t1")"
  else
    fact start_6379 "FAILED: $(tail -5 "$D/p6379/stdout.log" "$D/p6379/server.log" 2>/dev/null | tr '\n' ' ')"; return
  fi
  sleep 1
  fact rss_kb_ps_empty "$(rss_kb "$PID")"
  fact used_memory_rss_empty "$(info_field 6379 memory used_memory_rss)"
  fact used_memory_empty "$(info_field 6379 memory used_memory)"
  fact info_server "$($RESP --port 6379 INFO server | tr -d '\r' | grep -E '^(redis_version|valkey_version|server_name|os|arch_bits|process_id|tcp_port|executable|config_file|multiplexing_api):' | tr '\n' ' ')"
  fact config_dir_save_aof "$($RESP --port 6379 CONFIG GET dir | tr '\n' ' ') $($RESP --port 6379 CONFIG GET save | tr '\n' ' ') $($RESP --port 6379 CONFIG GET appendonly | tr '\n' ' ')"
  if command -v lsof >/dev/null; then
    fact listening "$(lsof -nP -a -p "$PID" -iTCP -sTCP:LISTEN 2>/dev/null | tail -n +2 | awk '{print $9}' | tr '\n' ' ')"
  else
    fact listening "$(ss -ltnp 2>/dev/null | grep "pid=$PID," | awk '{print $4}' | tr '\n' ' ')"
  fi
  fact pidfile_written "$([ -f "$D/p6379/valkey.pid" ] && echo "yes: $(cat "$D/p6379/valkey.pid")" || echo no)"

  # --- 2. stop with valkey-cli shutdown
  say "probe 2: valkey-cli shutdown nosave"
  t0="$(now)"
  "$CLI" -h 127.0.0.1 -p 6379 shutdown nosave >/dev/null 2>&1
  if wait_exit "$PID" 20; then
    t1="$(now)"; fact stop_cli_ms "$(ms "$t0" "$t1")"; fact stop_cli_exit_code "$EXITRC"
  else fact stop_cli "TIMEOUT"; kill -9 "$PID" 2>/dev/null; fi
  fact stop_cli_port_free "$(port_open 6379 && echo NO || echo yes)"
  fact stop_cli_pidfile_removed "$([ -f "$D/p6379/valkey.pid" ] && echo NO || echo yes)"

  # --- 3. stop with SIGTERM
  say "probe 3: SIGTERM"
  start_bg 6379 "$D/p6379" --save "" --appendonly no
  $RESP --port 6379 --wait 20 PING >/dev/null 2>&1
  t0="$(now)"; kill -TERM "$PID"
  if wait_exit "$PID" 20; then
    t1="$(now)"; fact stop_sigterm_ms "$(ms "$t0" "$t1")"; fact stop_sigterm_exit_code "$EXITRC"
  else fact stop_sigterm "TIMEOUT"; kill -9 "$PID" 2>/dev/null; fi
  fact stop_sigterm_port_free "$(port_open 6379 && echo NO || echo yes)"
  fact stop_sigterm_pidfile_removed "$([ -f "$D/p6379/valkey.pid" ] && echo NO || echo yes)"

  # --- 4. non-default port with a data dir: save on shutdown, recover on start
  say "probe 4: port 6390 with a data dir"
  start_bg 6390 "$D/p6390" --save "1 1" --appendonly no
  $RESP --port 6390 --wait 20 PING >/dev/null 2>&1
  $RESP --port 6390 SET kv_probe hello >/dev/null
  "$CLI" -p 6390 shutdown >/dev/null 2>&1; wait_exit "$PID" 20
  fact persist_dump_file "$(ls -la "$D/p6390/dump.rdb" 2>&1 | awk '{print $5" bytes"}')"
  start_bg 6390 "$D/p6390" --save "1 1" --appendonly no
  $RESP --port 6390 --wait 20 PING >/dev/null 2>&1
  fact persist_recovered_value "$($RESP --port 6390 GET kv_probe)"
  "$CLI" -p 6390 shutdown nosave >/dev/null 2>&1; wait_exit "$PID" 20

  # --- 5. orphan: the parent is SIGKILLed while the server runs
  say "probe 5: orphan after SIGKILL of the parent"
  mkdir -p "$D/p6391"
  bash -c '"$0" "$@" & wait' "$SERVER" --bind 127.0.0.1 --port 6391 --dir "$D/p6391" \
    --pidfile "$D/p6391/valkey.pid" --logfile "$D/p6391/server.log" --save "" --appendonly no >/dev/null 2>&1 &
  local parent=$!
  $RESP --port 6391 --wait 20 PING >/dev/null 2>&1
  local spid; spid="$(cat "$D/p6391/valkey.pid" 2>/dev/null)"
  fact orphan_server_pid_from_pidfile "$spid (parent shell was $parent)"
  kill -9 "$parent"; sleep 1
  fact orphan_server_alive "$(kill -0 "$spid" 2>/dev/null && echo yes || echo NO)"
  fact orphan_ppid_now "$(ps -o ppid= -p "$spid" | tr -d ' ')"
  fact orphan_port_held "$(port_open 6391 && echo yes || echo no)"
  fact orphan_pidfile_still_there "$([ -f "$D/p6391/valkey.pid" ] && echo yes || echo no)"
  # A new start on the same port while the orphan lives:
  start_bg 6391 "$D/p6391b" --save "" --appendonly no
  if wait_exit "$PID" 10; then
    fact orphan_second_start "exit code $EXITRC; log: $(grep -ih -E 'address|bind|error' "$D/p6391b/server.log" "$D/p6391b/stdout.log" 2>/dev/null | head -2 | tr '\n' ' ')"
  else fact orphan_second_start "still running (took the port?)"; kill -9 "$PID" 2>/dev/null; fi
  # Recovery: ask the orphan to stop, start again.
  "$CLI" -p 6391 shutdown nosave >/dev/null 2>&1
  local i=0; while kill -0 "$spid" 2>/dev/null && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
  fact orphan_recover_stop_by_cli "$(kill -0 "$spid" 2>/dev/null && echo NOT-STOPPED || echo stopped), port free: $(port_open 6391 && echo NO || echo yes)"
  start_bg 6391 "$D/p6391" --save "" --appendonly no
  fact orphan_restart_after_recover "$($RESP --port 6391 --wait 20 PING 2>&1)"
  # A stale pid file after SIGKILL of the server itself:
  kill -9 "$PID"; wait "$PID" 2>/dev/null
  fact sigkill_server_stale_pidfile "$([ -f "$D/p6391/valkey.pid" ] && echo "left behind: $(cat "$D/p6391/valkey.pid")" || echo "removed")"
  fact sigkill_server_port_free "$(port_open 6391 && echo NO || echo yes)"
  start_bg 6391 "$D/p6391" --save "" --appendonly no
  fact sigkill_restart "$($RESP --port 6391 --wait 20 PING 2>&1), new pidfile: $(cat "$D/p6391/valkey.pid" 2>/dev/null)"
  "$CLI" -p 6391 shutdown nosave >/dev/null 2>&1; wait_exit "$PID" 10
  say "probe done"
}

laravel() {
  load_paths || return
  local D="$WORK/laravel-server"
  rm -rf "$D"
  start_bg 6379 "$D" --save "" --appendonly no
  if ! $RESP --port 6379 --wait 20 PING >/dev/null 2>&1; then fact laravel "server did not start"; return; fi
  bash "$KV/laravel/run.sh" 6379 "$WORK/laravel" "$OUT" "$LABEL"
  "$CLI" -p 6379 INFO commandstats 2>/dev/null | tr -d '\r' | grep -E '^cmdstat_' | cut -d: -f1 | sed 's/cmdstat_//' | tr '\n' ' ' >"$OUT/commands-used.txt"
  "$CLI" -p 6379 shutdown nosave >/dev/null 2>&1; wait_exit "$PID" 20
}

summary() {
  echo "### $LABEL"
  [ -f "$FACTS" ] && sed 's/^/    /' "$FACTS"
  local j="$OUT/laravel-$LABEL.jsonl"
  if [ -f "$j" ]; then
    echo "Laravel: $(grep -c '"ok":true' "$j") passed, $(grep -c '"ok":false' "$j") failed"
    grep '"ok":false' "$j" | sed 's/^/    FAIL /'
  fi
}

cmd="${1:-}"; shift || true
case "$cmd" in
  obtain)
    case "${1:-}" in
      official) obtain_official ;;
      build) obtain_build ;;
      build-static) obtain_build_static ;;
      *) echo "obtain official|build|build-static" >&2; exit 2 ;;
    esac
    fact label "$LABEL"; fact target "$(target)"; fact valkey_version "$V"
    ;;
  probe) probe ;;
  laravel) laravel ;;
  summary) summary ;;
  *) echo "usage: kv.sh obtain|probe|laravel|summary" >&2; exit 2 ;;
esac
exit 0
