#!/bin/bash
# Part 1: does Caddy survive the death of its parent?
#
#   run-matrix.sh matrix [D1 D5 ...]   death modes x 8 option combinations
#   run-matrix.sh idle                 30 s without requests, stdio pipe
#   run-matrix.sh stdin                a stdin pipe as a guard (P4)
#
# The table goes to results/death.md (matrix) or results/prevention.md.
set -u
. "$(dirname "$0")/lib.sh"
set -m   # job control: each background job gets its own process group

MODE="${1:-matrix}"
[ $# -gt 0 ] && shift
ONLY="$*"
PIDS=""

# One run: start the parent, kill or let it die, and watch Caddy.
#   $1 case id   $2 group   $3 stdio   $4 kill-on-drop   $5 --die value
#   $6 how: kill9 | term | intgrp | self   $7 binary
# Prints one markdown row.
run_one() {
  local id="$1" group="$2" stdio="$3" kod="$4" die="$5" how="$6" bin="$7"
  local out="${WORK}/parent.out" pp cp s1 s2 s3 ps5 note=""

  fresh_work
  # `perl` resets SIGINT and SIGQUIT to their defaults before `exec`. A shell
  # that was itself started in the background hands them down as "ignored",
  # which would make the parent immune to the Ctrl+C of case D3.
  perl -e '$SIG{INT} = "DEFAULT"; $SIG{QUIT} = "DEFAULT"; exec @ARGV' -- \
    "${bin}" parent --group "${group}" --stdio "${stdio}" --kill-on-drop "${kod}" \
    --pidfile ours --die "${die}" >"${out}" 2>&1 &
  pp=$!
  if ! wait_for_line "${out}" READY 15; then
    echo "| ${id} | ${group} | ${stdio} | ${kod} | START FAILED | | | |"
    stop_pids "${pp}"
    return 1
  fi
  cp="$(field "${out}" caddy)"

  case "${how}" in
    kill9) /bin/sleep 1; kill -9 "${pp}" ;;
    term) /bin/sleep 1; kill -TERM "${pp}" ;;
    intgrp) /bin/sleep 1; kill -INT -- "-${pp}" ;;
    self)
      # The parent dies by itself one second after READY (D7: it stays alive).
      if [ "${die}" = panic-task ]; then
        wait_for_line "${out}" DAEMON_SAW 10 || note="no DAEMON_SAW line; "
      else
        local i
        for i in $(seq 1 100); do is_alive "${pp}" || break; /bin/sleep 0.1; done
      fi
      ;;
  esac
  # In D7 the parent stays alive, so there is nothing to wait for.
  if [ "${die}" != panic-task ]; then { wait "${pp}"; } 2>/dev/null; fi

  /bin/sleep 1
  s1="$(state_of "${cp}")"
  curl -s -m 2 -o /dev/null "${APP_URL}"
  /bin/sleep 0.4
  s2="$(state_of "${cp}")"
  # About five seconds after the death in total.
  /bin/sleep 3.5
  s3="$(state_of "${cp}")"
  ps5="$(state_of "${pp}")"

  if [ "${die}" = panic-task ]; then
    note="${note}daemon: $(grep DAEMON_SAW "${out}" | head -1 | sed 's/DAEMON_SAW //'); parent after 5s: ${ps5}; "
  fi
  if grep -q "panicked" "${out}" 2>/dev/null; then note="${note}panic message printed; "; fi

  echo "| ${id} | ${group} | ${stdio} | ${kod} | ${s1} | ${s2} | ${s3} | ${note} |"

  stop_pids "${cp}" "${pp}"
  if ! wait_ports_free 10; then
    echo "ports still busy after cleanup; stopping" >&2
    exit 2
  fi
}

matrix() {
  local results="${ROOT}/results/death.md"
  mkdir -p "${ROOT}/results"
  {
    echo "# Death matrix (macOS arm64, Caddy $("${CADDY}" version | cut -d' ' -f1))"
    echo
    echo "State of Caddy after the parent died: 1 s later, after one request, and 5 s later."
    echo "\`alive(ppid=1)\` means launchd adopted it. \`dead\` means it exited with its parent."
    echo
    echo "| Case | group | stdio | kill_on_drop | +1 s | after request | +5 s | notes |"
    echo "| --- | --- | --- | --- | --- | --- | --- | --- |"
  } >"${results}"

  local group stdio kod id die how bin spec
  for spec in "D1 none kill9" "D2 none term" "D3 none intgrp" "D4 panic-main self" \
              "D5 panic-main self" "D6 exit self" "D7 panic-task self" "D8 process-exit self"; do
    set -- ${spec}
    id="$1" die="$2" how="$3"
    if [ -n "${ONLY}" ] && ! echo " ${ONLY} " | grep -q " ${id} "; then continue; fi
    bin="${S06}"
    [ "${id}" = D5 ] && bin="${S06_ABORT}"
    for group in inherit own; do
      for stdio in file pipe; do
        for kod in on off; do
          echo "$(date +%T) run ${id} ${group} ${stdio} ${kod}" >&2
          run_one "${id}" "${group}" "${stdio}" "${kod}" "${die}" "${how}" "${bin}" >>"${results}"
        done
      done
    done
  done
  echo "wrote ${results}" >&2
}

# P3 follow-up: does an idle Caddy on a dead pipe survive 30 s without requests?
idle() {
  local out="${WORK}/parent.out" pp cp
  fresh_work
  "${S06}" parent --group own --stdio pipe --kill-on-drop off --die none >"${out}" 2>&1 &
  pp=$!
  wait_for_line "${out}" READY 15 || { echo "start failed"; return 1; }
  cp="$(field "${out}" caddy)"
  /bin/sleep 1
  kill -9 "${pp}"; { wait "${pp}"; } 2>/dev/null
  echo "- idle check, stdio pipe, parent SIGKILLed, no requests:"
  local t
  for t in 1 10 20 30; do
    /bin/sleep $(( t == 1 ? 1 : 10 ))
    echo "  - +${t} s: $(state_of "${cp}")"
  done
  curl -s -m 2 -o /dev/null "${APP_URL}"
  /bin/sleep 0.5
  echo "  - after the first request: $(state_of "${cp}")"
  stop_pids "${cp}" "${pp}"
  wait_ports_free 10
}

# P4: a stdin pipe that the parent holds. Does Caddy notice when it closes?
stdin_guard() {
  local out="${WORK}/parent.out" pp cp
  fresh_work
  "${S06}" parent --group own --stdio file --stdin pipe --kill-on-drop off --die none >"${out}" 2>&1 &
  pp=$!
  wait_for_line "${out}" READY 15 || { echo "start failed"; return 1; }
  cp="$(field "${out}" caddy)"
  /bin/sleep 1
  kill -9 "${pp}"; { wait "${pp}"; } 2>/dev/null
  /bin/sleep 3
  echo "- stdin pipe held by the parent, parent SIGKILLed: Caddy $(state_of "${cp}") after 3 s"
  stop_pids "${cp}" "${pp}"
  wait_ports_free 10
}

preflight
case "${MODE}" in
  matrix) matrix ;;
  idle) idle ;;
  stdin) stdin_guard ;;
  *) echo "usage: $0 matrix|idle|stdin" >&2; exit 1 ;;
esac
left="$(leftovers)"
if [ -n "${left}" ]; then echo "LEFTOVERS:"; echo "${left}"; else echo "no leftovers" >&2; fi
