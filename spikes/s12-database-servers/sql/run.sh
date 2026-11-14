#!/usr/bin/env bash
# S-12a spike, Linux and macOS: one candidate from start to finish.
#
#   run.sh <candidate-id> <out-dir>
#
# Looks up <candidate-id> for this platform in candidates.txt, then:
# download, verify, unpack, inspect, init a data dir, start on 127.0.0.1 on a
# non-default port, smoke query, measure memory, stop in several ways (and
# restart after each), port conflict, default port, orphan test.
# Writes <out-dir>/<id>.tsv (key<TAB>value) and <out-dir>/<id>.log.
#
# Throwaway code. It starts database servers: run it only on a CI runner or
# another machine you can throw away. bash 3.2 compatible (macOS).
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
ID="${1:?candidate id}"
OUT="${2:?out dir}"
PY="$(command -v python3 || command -v python)"
TOOL="$PY $HERE/tool.py"
mkdir -p "$OUT"
RES="$OUT/$ID.tsv"
: >"$RES"
exec > >(tee "$OUT/$ID.log") 2>&1

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }
rec() { # rec key value  (one line, tabs and newlines flattened)
  printf '%s\t%s\n' "$1" "$(printf '%s' "$2" | tr '\n\t' '  ' | cut -c1-700)" >>"$RES"
  log "  $1 = $(printf '%s' "$2" | tr '\n\t' '  ' | cut -c1-200)"
}
ms() { $TOOL ms; }
file_size() { wc -c <"$1" | tr -d ' '; }
sha256_of() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
md5_of() { if command -v md5sum >/dev/null; then md5sum "$1" | cut -d' ' -f1; else md5 -q "$1"; fi; }
alive() { # running, and not a zombie waiting to be reaped
  local st
  st="$(ps -o stat= -p "$1" 2>/dev/null | tr -d ' ')"
  [ -n "$st" ] && [ "${st#Z}" = "$st" ]
}

# ---------------------------------------------------------------- platform
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) PLAT=linux-x64 ;;
  Linux-aarch64 | Linux-arm64) PLAT=linux-arm64 ;;
  Darwin-arm64) PLAT=darwin-arm64 ;;
  Darwin-x86_64) PLAT=darwin-x64 ;;
  *) PLAT=unknown ;;
esac
OS="$(uname -s)"

ROW="$(grep -v '^#' "$HERE/candidates.txt" | awk -F'|' -v id="$ID" -v p="$PLAT" '$1==id && $2==p')"
rec id "$ID"
rec platform "$PLAT"
rec runner "$(uname -srm) $(if [ "$OS" = Darwin ]; then sw_vers -productVersion; else . /etc/os-release 2>/dev/null && echo "$PRETTY_NAME"; fi)"
if [ -z "$ROW" ]; then
  rec status "no candidate for this platform"
  exit 0
fi
IFS='|' read -r _ _ SERVER VERSION FORMAT URL CK_KIND CK_URL SIG_URL <<<"$ROW"
rec server "$SERVER"
rec version "$VERSION"
rec url "$URL"

BASE="${S12_WORK:-${RUNNER_TEMP:-/tmp}/s12}/$ID"
rm -rf "$BASE"
mkdir -p "$BASE/dl" "$BASE/unpack" "$BASE/gnupg"
chmod 700 "$BASE/gnupg"
W="$BASE"
DATA="$W/data"
SOCK="$(mktemp -d /tmp/s12s.XXXXXX)" # short path: unix socket paths are limited to ~104 bytes

cleanup() {
  for p in "${SRV_PID:-}" "${PARENT:-}" "${CHILD:-}" "${HP:-}" "${HOLD_PID:-}" "${CP:-}" "${SP:-}"; do
    [ -n "$p" ] && kill -KILL "$p" 2>/dev/null
  done
  pkill -KILL -f "$BASE/" 2>/dev/null
  true
}
trap cleanup EXIT

# ---------------------------------------------------------------- download and verify
FILE="$W/dl/$(basename "$URL")"
t0=$(ms)
if curl -fsSL --retry 5 --retry-delay 5 --max-time 900 -o "$FILE" "$URL"; then rec download_ok yes; else
  rec download_ok no
  rec status "download failed"
  exit 0
fi
rec download_ms $(($(ms) - t0))
rec archive_bytes "$(file_size "$FILE")"
SHA="$(sha256_of "$FILE")"
rec sha256_computed "$SHA"

rec checksum_kind "$CK_KIND"
rec checksum_url "$CK_URL"
case "$CK_KIND" in
  sha256file)
    want="$(curl -fsSL --max-time 60 "$CK_URL" | awk '{print $1}' | head -1)"
    if [ "$want" = "$SHA" ]; then rec checksum_ok yes; else rec checksum_ok "NO (published: $want)"; fi
    ;;
  md5file)
    want="$(curl -fsSL --max-time 60 "$CK_URL" | awk '{print $1}' | head -1)"
    got="$(md5_of "$FILE")"
    if [ "$want" = "$got" ]; then rec checksum_ok yes; else rec checksum_ok "NO (published: $want, got: $got)"; fi
    ;;
  ghdigest)
    repo="${CK_URL%%:*}"; rest="${CK_URL#*:}"; tag="${rest%%:*}"; asset="${rest#*:}"
    want="$(gh api "repos/$repo/releases/tags/$tag" --jq ".assets[] | select(.name==\"$asset\") | .digest" 2>/dev/null | sed 's/^sha256://')"
    if [ -n "$want" ] && [ "$want" = "$SHA" ]; then rec checksum_ok yes; else rec checksum_ok "NO (digest: ${want:-none})"; fi
    ;;
  *) rec checksum_ok "none published" ;;
esac

# GPG signature, best effort. MySQL: key from repo.mysql.com. Others: keyserver.
rec signature_url "$SIG_URL"
if [ "$SIG_URL" != "-" ]; then
  if command -v gpg >/dev/null; then
    export GNUPGHOME="$W/gnupg"
    curl -fsSL --max-time 60 -o "$W/dl/sig.asc" "$SIG_URL"
    out="$(gpg --batch --status-fd 1 --verify "$W/dl/sig.asc" "$FILE" 2>&1)"
    if echo "$out" | grep -q 'NO_PUBKEY'; then
      keyid="$(echo "$out" | awk '/NO_PUBKEY/{print $3; exit}')"
      if [ "$SERVER" = mysql ]; then
        curl -fsSL --max-time 60 https://repo.mysql.com/RPM-GPG-KEY-mysql-2023 | gpg --batch --import >/dev/null 2>&1
      else
        gpg --batch --keyserver hkps://keyserver.ubuntu.com --recv-keys "$keyid" >/dev/null 2>&1
      fi
      out="$(gpg --batch --status-fd 1 --verify "$W/dl/sig.asc" "$FILE" 2>&1)"
    fi
    if echo "$out" | grep -q 'GOODSIG'; then
      rec gpg_status "good signature, key $(echo "$out" | awk '/VALIDSIG/{print $3; exit}')"
    elif echo "$out" | grep -q 'NO_PUBKEY'; then
      rec gpg_status "signature present, public key not found (key id $keyid)"
    else
      rec gpg_status "BAD or unreadable: $(echo "$out" | head -2)"
    fi
    unset GNUPGHOME
  else
    rec gpg_status "no gpg on this runner"
  fi
else
  rec gpg_status "no signature published"
fi

# ---------------------------------------------------------------- unpack
U="$W/unpack"
t0=$(ms)
case "$FORMAT" in
  tar.gz | tar.xz) tar -xf "$FILE" -C "$U" ;;
  zip) unzip -q "$FILE" -d "$U" ;;
  jar)
    unzip -q "$FILE" -d "$W/jar"
    tar -xJf "$(find "$W/jar" -name '*.txz' | head -1)" -C "$U"
    ;;
  dmg)
    MNT="$(mktemp -d /tmp/s12mnt.XXXXXX)"
    yes | hdiutil attach -nobrowse -readonly -mountpoint "$MNT" "$FILE" >/dev/null 2>&1
    app="$(find "$MNT" -maxdepth 1 -name '*.app' | head -1)"
    ver="$(find "$app/Contents/Versions" -maxdepth 1 -mindepth 1 -type d ! -name latest | head -1)"
    cp -R "$ver" "$U/pgapp"
    hdiutil detach "$MNT" -force >/dev/null 2>&1
    ;;
esac
rec unpack_ms $(($(ms) - t0))
MAIN=mysqld
[ "$SERVER" = postgres ] && MAIN=postgres
BIN="$(find "$U" -maxdepth 6 -type f -name "$MAIN" -path '*/bin/*' | awk '{print length, $0}' | sort -n | head -1 | cut -d' ' -f2-)"
if [ -z "$BIN" ]; then
  rec status "no bin/$MAIN found after unpacking"
  exit 0
fi
ROOT="$(dirname "$(dirname "$BIN")")"
rec unpacked_bytes $(($(du -sk "$U" | cut -f1) * 1024))
rec unpacked_files "$(find "$U" -type f | wc -l | tr -d ' ')"
rec install_root "${ROOT#"$U"/}"
rec bin_files "$(ls "$ROOT/bin" | wc -l | tr -d ' ') files: $(ls "$ROOT/bin" | head -40 | tr '\n' ' ')"
rec license_files "$(find "$ROOT" "$(dirname "$ROOT")" -maxdepth 2 \( -iname 'LICENSE*' -o -iname 'COPYING*' -o -iname 'COPYRIGHT*' \) 2>/dev/null | sed "s|$U/||" | sort -u | head -8 | tr '\n' ' ')"

# ---------------------------------------------------------------- inspect the binaries
if [ "$SERVER" = mysql ]; then PROGS="mysqld mysql mysqladmin"; else PROGS="postgres initdb pg_ctl psql"; fi
rec client_tools "$(for p in $PROGS; do [ -x "$ROOT/bin/$p" ] && printf '%s ' "$p" || printf 'NO-%s ' "$p"; done)"
if [ "$OS" = Darwin ]; then
  rec quarantine_xattrs "$(xattr -lr "$ROOT" 2>/dev/null | grep -c 'com.apple.quarantine')"
  deps=""
  for p in $PROGS; do [ -x "$ROOT/bin/$p" ] && deps="$deps
$(otool -L "$ROOT/bin/$p" | tail -n +2 | awk '{print $1}')"; done
  for f in $(find "$ROOT/lib" -maxdepth 1 -name '*.dylib' -type f 2>/dev/null | head -40); do deps="$deps
$(otool -L "$f" | tail -n +2 | awk '{print $1}')"; done
  deps="$(echo "$deps" | sort -u | grep -v '^$')"
  rec dylib_system "$(echo "$deps" | grep -cE '^(/usr/lib/|/System/)')"
  rec dylib_relative "$(echo "$deps" | grep -cE '^@')"
  rec dylib_other_absolute "$(echo "$deps" | grep -vE '^(/usr/lib/|/System/|@)' | head -8 | tr '\n' ' ')"
  rec minos "$(otool -l "$BIN" | awk '/LC_BUILD_VERSION/{f=1} f&&/minos/{print $2; exit}')"
  rec archs "$(lipo -archs "$BIN" 2>/dev/null)"
  rec codesign "$(codesign -dv --verbose=2 "$BIN" 2>&1 | grep -E 'Authority=|TeamIdentifier|Signature=|flags=' | head -4 | tr '\n' ' ')"
else
  missing=""
  maxglibc=""
  for p in $PROGS; do
    [ -x "$ROOT/bin/$p" ] || continue
    m="$(ldd "$ROOT/bin/$p" 2>&1 | grep -E 'not found' | awk '{print $1}' | tr '\n' ' ')"
    [ -n "$m" ] && missing="$missing $p:$m"
    g="$(objdump -T "$ROOT/bin/$p" 2>/dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sort -uV | tail -1)"
    maxglibc="$maxglibc
$g"
  done
  for f in $(find "$ROOT/lib" -maxdepth 1 -name '*.so*' -type f 2>/dev/null | head -60); do
    maxglibc="$maxglibc
$(objdump -T "$f" 2>/dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sort -uV | tail -1)"
  done
  rec ldd_missing "${missing:-none}"
  rec glibc_needed_max "$(echo "$maxglibc" | grep GLIBC | sort -uV | tail -1)"
  rec glibcxx_needed_max "$(objdump -T "$BIN" 2>/dev/null | grep -o 'GLIBCXX_[0-9][0-9.]*' | sort -uV | tail -1)"
  rec ldd_system_libs "$(ldd "$BIN" 2>&1 | awk '{print $1}' | grep -v "^$" | tr '\n' ' ')"
  # Other distributions: only `--version`, in containers, no install.
  if [ "${S12_DOCKER:-0}" = 1 ] && command -v docker >/dev/null; then
    for img in ubuntu:22.04 debian:12 almalinux:9 alpine:3.20; do
      o="$(docker run --rm -v "$ROOT:/opt/db:ro" "$img" /opt/db/bin/$MAIN --version 2>&1 | head -2 | tr '\n' ' ')"
      case "$o" in
        *"$MAIN"*"Ver "* | *"PostgreSQL"*) rec "compat_$img" "ok: $o" ;;
        *) rec "compat_$img" "FAIL: $o" ;;
      esac
    done
  fi
fi
VER_OUT="$("$ROOT/bin/$MAIN" --version 2>&1 | head -1)"
rec server_version_output "$VER_OUT"

# ---------------------------------------------------------------- server commands
PORT=$($TOOL freeport)
[ "$SERVER" = mysql ] && DEFAULT_PORT=3306 || DEFAULT_PORT=5432
rec port_used "$PORT"
if [ "$SERVER" = mysql ]; then
  PIDFILE="$W/mysqld.pid"
  SRVLOG="$W/mysqld.err"
  srv_cmd() { # srv_cmd PORT -> SRV_CMD array
    SRV_CMD=("$ROOT/bin/mysqld" --no-defaults --basedir="$ROOT" --datadir="$DATA" --port="$1"
      --bind-address=127.0.0.1 --mysqlx=OFF --socket="$SOCK/mysql.sock" --pid-file="$PIDFILE"
      --log-error="$SRVLOG")
  }
  do_init() {
    "$ROOT/bin/mysqld" --no-defaults --initialize-insecure --basedir="$ROOT" --datadir="$DATA" --log-error="$W/init.err"
  }
  RECOVERY_RE='crash recovery|Crash recovery|was not properly shut down'
  CLEAN_RE='Shutdown complete'
  ready() { $TOOL wait 127.0.0.1 "$1" "$2"; }
  smoke() {
    [ -x "$ROOT/bin/mysql" ] || { echo "no mysql client in this archive"; return 3; }
    "$ROOT/bin/mysql" --no-defaults -uroot -h127.0.0.1 -P"$1" --protocol=tcp --default-character-set=utf8mb4 -N -e \
      "select version(); create database if not exists marsa_smoke; use marsa_smoke; create table if not exists t(id int primary key, name varchar(20)); insert into t values (1,'مرسى') on duplicate key update name=name; select id,name from t; select @@bind_address, @@port;"
  }
  hold_conn() { # an idle client session; sets HOLD_PID
    HOLD_PID=""
    if [ -x "$ROOT/bin/mysql" ]; then
      "$ROOT/bin/mysql" --no-defaults -uroot -h127.0.0.1 -P"$1" --protocol=tcp -e "select sleep(600)" >/dev/null 2>&1 &
      HOLD_PID=$!
    fi
  }
else
  PIDFILE="$DATA/postmaster.pid"
  SRVLOG="$W/stdout.log" # postgres logs to stderr
  srv_cmd() {
    SRV_CMD=("$ROOT/bin/postgres" -D "$DATA" -p "$1" -c listen_addresses=127.0.0.1 -c unix_socket_directories="$SOCK")
  }
  do_init() { "$ROOT/bin/initdb" -D "$DATA" -U postgres -A trust -E UTF8 --locale=C; }
  RECOVERY_RE='automatic recovery in progress|not properly shut down|was interrupted'
  CLEAN_RE='database system is shut down'
  ready() { $TOOL pgwait 127.0.0.1 "$1" "$2"; }
  smoke() {
    $TOOL pgquery 127.0.0.1 "$1" postgres postgres \
      "select version()" \
      "create table if not exists t(id int primary key, name text)" \
      "insert into t values (1,'مرسى') on conflict do nothing" \
      "select id,name from t" \
      "select current_setting('listen_addresses'), inet_server_port()"
  }
  hold_conn() {
    $TOOL pghold 127.0.0.1 "$1" postgres postgres >/dev/null 2>&1 &
    HOLD_PID=$!
  }
fi

# ---------------------------------------------------------------- helpers: processes, listeners, memory
descendants() { # descendants PID -> PID and all its children, one per line
  ps -e -o pid=,ppid= | awk -v root="$1" '{p[$1]=$2; n[++c]=$1} END{seen[root]=1; ch=1; while(ch){ch=0; for(i=1;i<=c;i++){ if(!(n[i] in seen) && (p[n[i]] in seen)){seen[n[i]]=1; ch=1}}} for(k in seen) print k}'
}
tree_rss_kb() { local s=0 p r; for p in $(descendants "$1"); do r=$(ps -o rss= -p "$p" 2>/dev/null | tr -d ' '); s=$((s + ${r:-0})); done; echo $s; }
tree_pss_kb() { local s=0 p r; for p in $(descendants "$1"); do [ -r "/proc/$p/smaps_rollup" ] && r=$(awk '/^Pss:/{s+=$2} END{print s+0}' "/proc/$p/smaps_rollup") && s=$((s + r)); done; echo $s; }
listeners() { # listeners PID -> listening TCP addresses of PID and its children
  local p
  for p in $(descendants "$1"); do
    if [ "$OS" = Linux ]; then
      ss -H -ltnp 2>/dev/null | grep "pid=$p," | awk '{print $4}'
    else
      lsof -nP -a -p "$p" -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR>1{print $9}'
    fi
  done | sort -u
}
wait_gone() { # wait_gone PID SECONDS
  local end=$((SECONDS + $2))
  while alive "$1" && [ $SECONDS -lt $end ]; do sleep 0.2; done
  ! alive "$1"
}

# ---------------------------------------------------------------- lifecycle
SRV_PID=""
start_srv() { # start_srv PORT TAG  -> SRV_PID; records start_<TAG>_*
  local port="$1" tag="$2" t0 before
  srv_cmd "$port"
  before=$(wc -c <"$SRVLOG" 2>/dev/null || echo 0)
  t0=$(ms)
  "${SRV_CMD[@]}" >>"$W/stdout.log" 2>&1 &
  SRV_PID=$!
  if ready "$port" 90 && alive "$SRV_PID"; then
    rec "start_${tag}_ready_ms" $(($(ms) - t0))
  else
    rec "start_${tag}_ready_ms" "FAILED: $(tail -c 400 "$SRVLOG" 2>/dev/null | tr '\n' ' ') $(tail -c 300 "$W/stdout.log" | tr '\n' ' ')"
    return 1
  fi
  rec "start_${tag}_recovery_log" "$(tail -c +$((before + 1)) "$SRVLOG" 2>/dev/null | grep -E "$RECOVERY_RE" | head -2 | tr '\n' ' ')"
  return 0
}

stop_srv() { # stop_srv METHOD TAG: stop SRV_PID by METHOD, recording how it went
  local m="$1" tag="$2" t0 rc held=""
  hold_conn "$PORT"
  sleep 1
  t0=$(ms)
  case "$m" in
    mysqladmin) "$ROOT/bin/mysqladmin" --no-defaults -h127.0.0.1 -P"$PORT" -uroot --protocol=tcp shutdown >/dev/null 2>&1 ;;
    pg_ctl_fast) "$ROOT/bin/pg_ctl" stop -m fast -D "$DATA" -t 30 >/dev/null 2>&1 ;;
    SIGTERM) kill -TERM "$SRV_PID" ;;
    SIGINT) kill -INT "$SRV_PID" ;;
    SIGQUIT) kill -QUIT "$SRV_PID" ;;
    SIGKILL) kill -KILL "$SRV_PID" ;;
  esac
  if wait_gone "$SRV_PID" 20; then
    wait "$SRV_PID" 2>/dev/null
    rc=$?
    rec "stop_${tag}_${m}" "exited in $(($(ms) - t0)) ms with an idle client connected, exit code $rc"
  else
    rec "stop_${tag}_${m}" "STILL RUNNING after 20 s with an idle client connected; escalating"
    kill -INT "$SRV_PID" 2>/dev/null
    wait_gone "$SRV_PID" 10 || kill -KILL "$SRV_PID" 2>/dev/null
    wait "$SRV_PID" 2>/dev/null
  fi
  if [ -n "$HOLD_PID" ]; then
    kill "$HOLD_PID" 2>/dev/null
    wait "$HOLD_PID" 2>/dev/null
  fi
  rec "stop_${tag}_clean_log" "$(grep -cE "$CLEAN_RE" "$SRVLOG" 2>/dev/null)"
  rec "stop_${tag}_leftovers" "pidfile=$([ -e "$PIDFILE" ] && echo yes || echo no) socket=$(ls "$SOCK" 2>/dev/null | tr '\n' ',')"
}

t0=$(ms)
mkdir -p "$W"
if do_init >"$W/init.out" 2>&1; then rec init_ok "yes in $(($(ms) - t0)) ms; data dir $(($(du -sk "$DATA" | cut -f1))) KB"; else
  rec init_ok "NO: $(tail -5 "$W/init.out" "$W/init.err" 2>/dev/null | tr '\n' ' ')"
  rec status "init failed"
  exit 0
fi

# 1. first start: smoke, memory, listeners
if start_srv "$PORT" first; then
  sleep 3
  rec rss_idle_kb "$(tree_rss_kb "$SRV_PID")"
  [ "$OS" = Linux ] && rec pss_idle_kb "$(tree_pss_kb "$SRV_PID")"
  rec process_count "$(descendants "$SRV_PID" | wc -l | tr -d ' ')"
  rec listeners "$(listeners "$SRV_PID" | tr '\n' ' ')"
  if [ -n "$(listeners "$SRV_PID" | grep -v '^127\.0\.0\.1:')" ]; then rec loopback_only "NO"; else rec loopback_only "yes"; fi
  rec smoke_output "$(smoke "$PORT" 2>&1 | tr '\n' ' ')"
  rec rss_after_smoke_kb "$(tree_rss_kb "$SRV_PID")"
  rec socket_files "$(ls "$SOCK" | tr '\n' ' ')"
  rec pidfile_content "$(head -1 "$PIDFILE" 2>/dev/null)"
else
  rec status "first start failed"
  exit 0
fi

# 2. stop methods, each followed by a restart on the same data dir
if [ "$SERVER" = mysql ]; then METHODS="mysqladmin SIGTERM SIGKILL"; else METHODS="pg_ctl_fast SIGINT SIGTERM SIGQUIT SIGKILL"; fi
n=0
for m in $METHODS; do
  n=$((n + 1))
  stop_srv "$m" "$m"
  if ! start_srv "$PORT" "after_$m"; then rec status "restart after $m failed"; exit 0; fi
done

# 3. port taken by someone else
stop_srv "$([ "$SERVER" = mysql ] && echo mysqladmin || echo pg_ctl_fast)" final1
$TOOL hold-port 127.0.0.1 "$PORT" >/dev/null 2>&1 &
HP=$!
sleep 1
srv_cmd "$PORT"
"${SRV_CMD[@]}" >"$W/conflict.out" 2>&1 &
CP=$!
if wait_gone "$CP" 40; then
  wait "$CP" 2>/dev/null
  rec port_conflict "server exited with code $? ; $(grep -hiE 'already in use|already running|bind|address' "$W/conflict.out" "$SRVLOG" 2>/dev/null | tail -2 | tr '\n' ' ')"
else
  rec port_conflict "server kept running although the port was taken"
  kill -KILL "$CP" 2>/dev/null
fi
kill "$HP" 2>/dev/null
wait "$HP" 2>/dev/null

# 4. default port (no admin rights needed: both are above 1024)
if $TOOL tcp 127.0.0.1 "$DEFAULT_PORT"; then
  rec default_port "$DEFAULT_PORT is already taken on this runner"
else
  if start_srv "$DEFAULT_PORT" default; then
    rec default_port "$DEFAULT_PORT works; listeners: $(listeners "$SRV_PID" | tr '\n' ' ')"
    PORT_SAVE=$PORT
    PORT=$DEFAULT_PORT
    stop_srv "$([ "$SERVER" = mysql ] && echo mysqladmin || echo pg_ctl_fast)" default
    PORT=$PORT_SAVE
  else
    rec default_port "$DEFAULT_PORT failed"
  fi
fi

# 5. orphan: the parent is killed with SIGKILL, the server is not told
orphan_start() { # orphan_start -> PARENT, CHILD (server pid)
  srv_cmd "$PORT"
  rm -f "$W/child.pid"
  bash -c '"$@" >>"$0/stdout.log" 2>&1 & echo $! >"$0/child.pid"; wait' "$W" "${SRV_CMD[@]}" &
  PARENT=$!
  ready "$PORT" 90
  CHILD="$(cat "$W/child.pid" 2>/dev/null)"
}
orphan_report() { # orphan_report PREFIX
  rec "$1_server_alive" "$(alive "$CHILD" && echo yes || echo no) (parent alive: $(alive "$PARENT" && echo yes || echo no); server ppid now: $(ps -o ppid= -p "$CHILD" 2>/dev/null | tr -d ' '))"
  rec "$1_pidfile" "$([ -e "$PIDFILE" ] && echo "present, pid $(head -1 "$PIDFILE")" || echo absent)"
  rec "$1_socket_files" "$(ls "$SOCK" 2>/dev/null | tr '\n' ' ')"
  rec "$1_port_held" "$($TOOL tcp 127.0.0.1 "$PORT" && echo yes || echo no)"
}

# 5a. parent dies, the server lives on
orphan_start
rec orphan_server_pid "$CHILD (parent $PARENT)"
kill -KILL "$PARENT" 2>/dev/null
wait "$PARENT" 2>/dev/null
sleep 1
orphan_report orphan_after_parent_kill
rec orphan_still_answers "$(smoke "$PORT" 2>&1 | head -2 | tr '\n' ' ')"
# a second marsad would try to start it again
srv_cmd "$PORT"
"${SRV_CMD[@]}" >"$W/second.out" 2>&1 &
SP=$!
if wait_gone "$SP" 40; then
  wait "$SP" 2>/dev/null
  rec orphan_second_start "refused, exit code $? ; $(grep -hiE 'lock|already|in use|FATAL|Unable|ERROR' "$W/second.out" "$SRVLOG" 2>/dev/null | tail -3 | tr '\n' ' ')"
else
  rec orphan_second_start "second server kept running (two servers on one data dir?)"
  kill -KILL "$SP" 2>/dev/null
fi
# recovery A: read the pid file, check, send SIGTERM/SIGINT (what marsad would do)
opid="$(head -1 "$PIDFILE" 2>/dev/null)"
rec orphan_pid_matches_pidfile "$([ "$opid" = "$CHILD" ] && echo yes || echo "NO ($opid vs $CHILD)")"
# mysqld: SIGTERM. postgres: SIGINT (fast); with no client SIGTERM would do too.
kill -"$([ "$SERVER" = mysql ] && echo TERM || echo INT)" "$opid" 2>/dev/null
if wait_gone "$CHILD" 30; then
  rec orphan_stopped_by_pidfile "yes; pidfile now: $([ -e "$PIDFILE" ] && echo present || echo removed); sockets: $(ls "$SOCK" 2>/dev/null | tr '\n' ' ')"
else
  rec orphan_stopped_by_pidfile "NO, still running after the signal"
  kill -KILL "$CHILD"
fi

# 5b. everything dies at once (a power cut or a hard kill of the whole tree)
orphan_start
rec orphan2_server_pid "$CHILD"
kill -KILL "$PARENT" 2>/dev/null
kill -KILL "$CHILD" 2>/dev/null
wait "$PARENT" 2>/dev/null
sleep 1
orphan_report orphan_hardkill
if start_srv "$PORT" after_hardkill; then
  rec orphan_hardkill_restart "ok"
  stop_srv "$([ "$SERVER" = mysql ] && echo mysqladmin || echo pg_ctl_fast)" end
else
  rec orphan_hardkill_restart "FAILED"
fi

rec status "done"
log "finished $ID"
exit 0
