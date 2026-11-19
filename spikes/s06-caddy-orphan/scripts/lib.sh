#!/bin/bash
# Shared helpers for run-matrix.sh and run-recovery.sh. Source this file.
# Rules: only processes whose executable path is one of our three binaries are
# ever signalled, and every kill names a pid that the script recorded. There is
# no `pkill`, no `killall`, and no `sudo`.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CADDY="${ROOT}/vendor/caddy"
# `S06_TARGET` points at another cargo target dir, so that a rebuild never
# replaces the binary of a script that is running.
S06="${S06_TARGET:-${ROOT}/target}/debug/s06"
S06_ABORT="${S06_TARGET:-${ROOT}/target}/abort/s06"
APP_PORT="${S06_APP_PORT:-18106}"
ADMIN_PORT="${S06_ADMIN_PORT:-2106}"
export S06_APP_PORT="${APP_PORT}" S06_ADMIN_PORT="${ADMIN_PORT}"
export S06_WORK="${S06_WORK:-${ROOT}/work}"
WORK="${S06_WORK}"
APP_URL="http://127.0.0.1:${APP_PORT}/"
PS=/bin/ps
# S-06b: the same scripts run on Linux. The results of each system go to a
# folder of their own, and the tables name the system.
if [ "$(uname -s)" = Linux ]; then
  LSOF=/usr/bin/lsof
  RESULTS="${S06_RESULTS:-${ROOT}/results/linux}"
  PLATFORM="Linux $(uname -m), $(. /etc/os-release && echo "${PRETTY_NAME}")"
else
  LSOF=/usr/sbin/lsof
  RESULTS="${ROOT}/results"
  PLATFORM="macOS $(uname -m)"
fi
mkdir -p "${RESULTS}/raw"

# The program a pid runs. macOS: `ps -o comm=` is the path it was started
# with. Linux: `comm` is only the first 15 bytes of the name, so the target
# of /proc/<pid>/exe.
exe_of() {
  if [ -e /proc/self/exe ]; then
    readlink "/proc/$1/exe" 2>/dev/null
  else
    "${PS}" -p "$1" -o comm= 2>/dev/null
  fi
}

# Whether something listens on a TCP port (127.0.0.1 or any address).
port_busy() {
  "${LSOF}" -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
}

# Waits up to $1 seconds for both ports to be free.
wait_ports_free() {
  local i
  for i in $(seq 1 $(( $1 * 10 ))); do
    if ! port_busy "${APP_PORT}" && ! port_busy "${ADMIN_PORT}"; then return 0; fi
    /bin/sleep 0.1
  done
  return 1
}

# Refuses to start when a port is taken. Nobody is killed.
preflight() {
  if [ ! -x "${CADDY}" ]; then echo "missing ${CADDY}: run scripts/fetch-caddy.sh" >&2; exit 1; fi
  if [ ! -x "${S06}" ] || [ ! -x "${S06_ABORT}" ]; then
    echo "build first: cargo build && cargo build --profile abort" >&2; exit 1
  fi
  local p
  for p in "${APP_PORT}" "${ADMIN_PORT}"; do
    if port_busy "$p"; then
      echo "port $p is busy; set S06_APP_PORT / S06_ADMIN_PORT. Nothing was killed." >&2
      exit 1
    fi
  done
}

# Prints "dead" or "alive(ppid=N)" for a pid. A zombie counts as dead.
state_of() {
  local out
  out="$("${PS}" -p "$1" -o ppid=,stat= 2>/dev/null)"
  if [ -z "${out}" ]; then echo "dead"; return; fi
  # shellcheck disable=SC2086
  set -- ${out}
  case "$2" in
    Z*) echo "dead(zombie)" ;;
    T*) echo "alive(ppid=$1,stopped)" ;;
    *) echo "alive(ppid=$1)" ;;
  esac
}

is_alive() {
  case "$(state_of "$1")" in dead*) return 1 ;; *) return 0 ;; esac
}

# Sends a signal to a pid only if its executable path is one of ours.
kill_if_ours() {
  local sig="$1" pid="$2" comm
  comm="$(exe_of "${pid}")"
  case "${comm}" in
    "${CADDY}" | "${S06}" | "${S06_ABORT}") kill -"${sig}" "${pid}" 2>/dev/null ;;
    *) return 1 ;;
  esac
}

# Stops recorded pids: CONT (in case they are frozen), TERM, then KILL.
stop_pids() {
  local pid
  for pid in "$@"; do
    [ -n "${pid}" ] && kill_if_ours CONT "${pid}"
  done
  for pid in "$@"; do
    [ -n "${pid}" ] && kill_if_ours TERM "${pid}"
  done
  /bin/sleep 0.3
  for pid in "$@"; do
    [ -n "${pid}" ] && is_alive "${pid}" && kill_if_ours KILL "${pid}"
  done
  return 0
}

# Final check: lists any process that still runs one of our binaries.
leftovers() {
  "${PS}" -axo pid= | while read -r pid; do
    comm="$(exe_of "${pid}")"
    case "${comm}" in
      "${CADDY}" | "${S06}" | "${S06_ABORT}") echo "${pid} ${comm}" ;;
    esac
  done
}

# Waits until a file contains a pattern. Returns 1 after $3 seconds.
wait_for_line() {
  local file="$1" pattern="$2" i
  for i in $(seq 1 $(( $3 * 10 ))); do
    if grep -q "${pattern}" "${file}" 2>/dev/null; then return 0; fi
    /bin/sleep 0.1
  done
  return 1
}

# Parses "key=value" out of a line of a file.
field() {
  sed -En "s/(^|.*[[:space:]])$2=([^[:space:]]*).*/\2/p" "$1" | head -1
}

fresh_work() {
  rm -rf "${WORK}"
  mkdir -p "${WORK}"
}
