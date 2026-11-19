#!/bin/bash
# Part 1: does Caddy survive the death of its parent?
#
#   run-matrix.sh matrix [D1 D5 ...]   death modes x 8 option combinations
#   run-matrix.sh idle                 30 s without requests, stdio pipe
#   run-matrix.sh stdin                a stdin pipe as a guard (P4)
#   run-matrix.sh pdeathsig            Linux: prctl(PR_SET_PDEATHSIG) (P7, S-06b)
#   run-matrix.sh systemd              Linux: the parent under systemd --user (S-06b)
#
# The table goes to death.md (matrix) or prevention.md in the results folder
# of the system (lib.sh).
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
  local results="${RESULTS}/death.md"
  {
    echo "# Death matrix (${PLATFORM}, Caddy $("${CADDY}" version | cut -d' ' -f1))"
    echo
    echo "State of Caddy after the parent died: 1 s later, after one request, and 5 s later."
    echo "\`alive(ppid=N)\` means the system adopted it: launchd (1) on macOS, pid 1 or the nearest subreaper on Linux (\`ppid of the orphan\` in prevention.md). \`dead\` means it exited with its parent."
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

# The name of the parent of a pid, for "who adopted the orphan".
adopter_of() {
  local ppid
  ppid="$("${PS}" -p "$1" -o ppid= 2>/dev/null | tr -d ' ')"
  [ -z "${ppid}" ] && { echo "-"; return; }
  echo "${ppid}:$("${PS}" -p "${ppid}" -o comm= 2>/dev/null)"
}

# P7 (Linux): PR_SET_PDEATHSIG=SIGTERM in Caddy before exec. The signal comes
# when the THREAD that spawned Caddy ends, not the process: tokio worker
# threads live as long as the runtime, a short thread does not.
#   $1 label  $2 spawn-from  $3 how the parent ends: kill9 | term | none
pdeathsig_one() {
  local label="$1" from="$2" how="$3" out="${WORK}/parent.out" pp cp s1 s5
  fresh_work
  "${S06}" parent --group own --stdio file --kill-on-drop off --pdeathsig on \
    --spawn-from "${from}" --die none >"${out}" 2>&1 &
  pp=$!
  if ! wait_for_line "${out}" READY 15; then
    echo "| ${label} | ${from} | ${how} | START FAILED: $(tail -1 "${out}") | | |"
    stop_pids "${pp}"; wait_ports_free 10; return
  fi
  cp="$(field "${out}" caddy)"
  /bin/sleep 1
  case "${how}" in
    kill9) kill -9 "${pp}"; { wait "${pp}"; } 2>/dev/null ;;
    term) kill -TERM "${pp}"; { wait "${pp}"; } 2>/dev/null ;;
    none) ;;
  esac
  /bin/sleep 1
  s1="$(state_of "${cp}")"
  /bin/sleep 4
  s5="$(state_of "${cp}")"
  echo "| ${label} | ${from} | ${how} | ${s1} | ${s5} | $(adopter_of "${cp}") |"
  stop_pids "${cp}" "${pp}"
  wait_ports_free 10
}

pdeathsig() {
  local results="${RESULTS}/prevention.md"
  {
    echo
    echo "## P7: PR_SET_PDEATHSIG (${PLATFORM})"
    echo
    echo "Caddy gets SIGTERM when the thread that spawned it ends. The parent keeps its tokio runtime."
    echo
    echo "| Case | spawned from | parent | Caddy +1 s | Caddy +5 s | ppid of Caddy |"
    echo "| --- | --- | --- | --- | --- | --- |"
    pdeathsig_one "P7a" task kill9
    pdeathsig_one "P7b" task term
    pdeathsig_one "P7c" task none
    pdeathsig_one "P7d" short-thread none
  } >>"${results}"
  echo "wrote ${results}" >&2
}

# Linux: the parent under systemd. Is Caddy killed with it when the parent
# dies, and by which KillMode? A desktop session starts apps in a scope;
# `marsa daemon start` from a terminal is in the terminal's scope.
#   $1 label  $2 kind: service | scope  $3 KillMode (service only)
systemd_one() {
  local label="$1" kind="$2" mode="$3" out="${WORK}/parent.out" pp cp s1 s5 unit
  fresh_work
  unit="s06-${label,,}-$$"
  case "${kind}" in
    service)
      systemd-run --user --quiet --unit="${unit}" --property=KillMode="${mode}" \
        --property=StandardOutput="file:${out}" --property=StandardError="file:${out}" \
        --setenv=S06_APP_PORT="${APP_PORT}" --setenv=S06_ADMIN_PORT="${ADMIN_PORT}" \
        --setenv=S06_WORK="${WORK}" \
        "${S06}" parent --group own --stdio file --kill-on-drop on --die none
      ;;
    scope)
      systemd-run --user --quiet --scope --unit="${unit}" \
        "${S06}" parent --group own --stdio file --kill-on-drop on --die none >"${out}" 2>&1 &
      ;;
  esac
  if ! wait_for_line "${out}" READY 15; then
    echo "| ${label} | ${kind} | ${mode} | START FAILED: $(tail -1 "${out}" 2>/dev/null) | | | |"
    systemctl --user stop "${unit}" >/dev/null 2>&1
    wait_ports_free 10; return
  fi
  pp="$(field "${out}" parent)"
  cp="$(field "${out}" caddy)"
  /bin/sleep 1
  kill -9 "${pp}"
  /bin/sleep 1
  s1="$(state_of "${cp}")"
  /bin/sleep 4
  s5="$(state_of "${cp}")"
  echo "| ${label} | ${kind} | ${mode} | ${s1} | ${s5} | $(adopter_of "${cp}") | $(systemctl --user is-active "${unit}" 2>/dev/null) |"
  stop_pids "${cp}" "${pp}"
  systemctl --user stop "${unit}" >/dev/null 2>&1
  systemctl --user reset-failed "${unit}" >/dev/null 2>&1
  wait_ports_free 10
}

systemd_cases() {
  local results="${RESULTS}/prevention.md"
  {
    echo
    echo "## systemd --user (${PLATFORM}, $(systemctl --version | head -1))"
    echo
    echo "The parent runs as a transient user unit and is SIGKILLed. \`kill_on_drop\` is on, so only systemd can kill Caddy here."
    echo
    echo "| Case | unit | KillMode | Caddy +1 s | Caddy +5 s | ppid of Caddy | unit after |"
    echo "| --- | --- | --- | --- | --- | --- | --- |"
    if ! systemctl --user is-system-running >/dev/null 2>&1 \
      && ! systemctl --user list-units >/dev/null 2>&1; then
      echo "| - | - | - | no systemd --user manager | | | |"
    else
      systemd_one "U1" service control-group
      systemd_one "U2" service mixed
      systemd_one "U3" service process
      systemd_one "U4" scope -
    fi
  } >>"${results}"
  echo "wrote ${results}" >&2
}

preflight
case "${MODE}" in
  matrix) matrix ;;
  idle) idle ;;
  stdin) stdin_guard ;;
  pdeathsig) pdeathsig ;;
  systemd) systemd_cases ;;
  *) echo "usage: $0 matrix|idle|stdin|pdeathsig|systemd" >&2; exit 1 ;;
esac
left="$(leftovers)"
if [ -n "${left}" ]; then echo "LEFTOVERS:"; echo "${left}"; else echo "no leftovers" >&2; fi
