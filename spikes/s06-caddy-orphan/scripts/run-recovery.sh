#!/bin/bash
# Part 2: can the next marsad find the orphan and take control?
#
#   run-recovery.sh recovery   cases R1..R7, three detection methods, reap and adopt
#   run-recovery.sh gap        request gap during reap and adopt, 5 runs each
#   run-recovery.sh pidfile    Caddy's own --pidfile against ours (P5)
#
# Results go to results/recovery.md and results/gap.md.
set -u
. "$(dirname "$0")/lib.sh"
set -m   # each background job gets its own process group

MODE="${1:-recovery}"
TRACK=""        # pids started by this script; only these are ever signalled
BYSTANDER=""    # a process the recovery must never touch

track() { TRACK="${TRACK} $*"; }

# Strips pids so that rows stay stable between runs.
mask() { sed -E 's/pid=[0-9]+/pid=N/g; s/\(([0-9]+)\)/(N)/g; s/Some\([0-9]+\)/Some(N)/g'; }

# Starts a parent, then SIGKILLs it. Sets ORPHAN (the Caddy pid) and PARENT.
make_orphan() {
  local out="${WORK}/parent.out"
  "${S06}" parent --group own --stdio file --kill-on-drop on --pidfile ours --die none >"${out}" 2>&1 &
  PARENT=$!
  wait_for_line "${out}" READY 15 || { echo "parent did not start" >&2; exit 1; }
  ORPHAN="$(field "${out}" caddy)"
  track "${ORPHAN}" "${PARENT}"
  /bin/sleep 0.5
  kill -9 "${PARENT}"; { wait "${PARENT}"; } 2>/dev/null
}

# A Caddy that is not ours: same binary, config without our @id.
#   $1 site port (default: ours)   $2 admin port (default: ours)
start_foreign_caddy() {
  local app="${1:-${APP_PORT}}" admin="${2:-${ADMIN_PORT}}"
  mkdir -p "${WORK}/foreign-config" "${WORK}/foreign-data"
  cat >"${WORK}/foreign.json" <<JSON
{
  "admin": { "listen": "127.0.0.1:${admin}", "config": { "persist": false } },
  "apps": { "http": { "servers": { "other": {
    "listen": ["127.0.0.1:${app}"],
    "automatic_https": { "disable": true },
    "routes": [{ "handle": [{ "handler": "static_response", "body": "foreign" }] }]
  } } } }
}
JSON
  XDG_CONFIG_HOME="${WORK}/foreign-config" XDG_DATA_HOME="${WORK}/foreign-data" \
    "${CADDY}" run --config "${WORK}/foreign.json" >"${WORK}/foreign.out" 2>&1 &
  FOREIGN=$!
  track "${FOREIGN}"
  wait_for_line "${WORK}/foreign.out" "serving initial configuration" 10 || { echo "foreign Caddy did not start" >&2; exit 1; }
}

write_pidfile() { # $1 pid  $2 exe  $3 started
  mkdir -p "${WORK}/run"
  printf '{"pid":%s,"exe":"%s","started":"%s"}' "$1" "$2" "$3" >"${WORK}/run/caddy.pid"
}

# --- setups: each sets BYSTANDER (empty when nothing must survive) -----------

setup_R1() { make_orphan; BYSTANDER=""; }
setup_R2() { make_orphan; rm -f "${WORK}/run/caddy.pid"; BYSTANDER=""; }
setup_R3() { make_orphan; kill -9 "${ORPHAN}"; /bin/sleep 0.5; BYSTANDER=""; }
setup_R4() {
  /bin/sleep 300 &
  BYSTANDER=$!
  track "${BYSTANDER}"
  write_pidfile "${BYSTANDER}" "${CADDY}" "$("${PS}" -p "${BYSTANDER}" -o lstart= | sed 's/ *$//')"
}
setup_R5() { start_foreign_caddy; BYSTANDER="${FOREIGN}"; }
setup_R5b() {
  start_foreign_caddy; BYSTANDER="${FOREIGN}"
  # The pid file names the foreign Caddy: right pid, right path, wrong start time.
  write_pidfile "${FOREIGN}" "${CADDY}" "Mon Jan  1 00:00:00 2024"
}
setup_R5c() { setup_R5b; }
# R5d/R5e: the pid file names a foreign Caddy on OTHER ports (a reused pid of
# our own binary), and another program holds our site port. Only the start
# time tells the foreign Caddy from our dead one.
setup_R5d() {
  local fa=$(( APP_PORT + 1000 )) fb=$(( ADMIN_PORT + 1000 ))
  if port_busy "${fa}" || port_busy "${fb}"; then echo "ports ${fa}/${fb} busy" >&2; exit 1; fi
  start_foreign_caddy "${fa}" "${fb}"
  BYSTANDER="${FOREIGN}"
  write_pidfile "${FOREIGN}" "${CADDY}" "Mon Jan  1 00:00:00 2024"
  "${S06}" squat --port "${APP_PORT}" >"${WORK}/squat.out" 2>&1 &
  track "$!"
  wait_for_line "${WORK}/squat.out" SQUAT_READY 5 || { echo "squat failed" >&2; exit 1; }
}
setup_R5e() { setup_R5d; }
setup_R6() {
  "${S06}" squat --port "${APP_PORT}" >"${WORK}/squat.out" 2>&1 &
  BYSTANDER=$!
  track "${BYSTANDER}"
  wait_for_line "${WORK}/squat.out" SQUAT_READY 5 || { echo "squat failed" >&2; exit 1; }
}
setup_R7() { make_orphan; kill -STOP "${ORPHAN}"; BYSTANDER=""; }
setup_R7i() { setup_R7; }

DESC_R1="orphan of ours, pid file intact"
DESC_R2="orphan of ours, pid file missing"
DESC_R3="pid file stale (Caddy dead)"
DESC_R4="pid file names a reused pid (a \`sleep\`)"
DESC_R5="foreign Caddy on both ports"
DESC_R5b="pid file names a foreign Caddy (same binary, wrong start time)"
DESC_R5c="same as R5b, but the pid file check compares only the path (no start time)"
DESC_R5d="pid file names a foreign Caddy of our binary on other ports, and a program holds our site port"
DESC_R5e="same as R5d, but the pid file check compares only the path (no start time)"
DESC_R6="another program holds the site port"
DESC_R7="orphan of ours, frozen (SIGSTOP)"
DESC_R7i="same as R7, but the pid file is ignored (pid comes from \`lsof\` on the admin port)"

steps_of() { # compresses the STEP lines of a recover run
  grep '^STEP' "$1" | sed -E 's/^STEP t_ms=([0-9]+) ([a-z_]+) ?(.*)$/\2[\3]@\1ms/' | mask | paste -sd'>' - | sed 's/>/ > /g'
}

# Runs one case with one action and appends rows to the two tables.
run_case() { # $1 case id  $2 action (reap|adopt)
  local id="$1" action="$2" out="${WORK}/recover.out" rc desc bystander_state orphan_state ports
  fresh_work
  TRACK=""
  ORPHAN=""
  eval "setup_${id}"
  eval "desc=\${DESC_${id}}"
  local t0 t1
  t0=$(perl -MTime::HiRes=time -e 'printf "%.3f", time')
  local flags=""
  { [ "${id}" = R5c ] || [ "${id}" = R5e ]; } && flags="--name-only"
  [ "${id}" = R7i ] && flags="--ignore-pidfile"
  # shellcheck disable=SC2086
  "${S06}" recover --action "${action}" --hold-secs 1 ${flags} >"${out}" 2>&1
  rc=$?
  t1=$(perl -MTime::HiRes=time -e 'printf "%.3f", time')

  local inspect pidfile admin app adm decision total_ms
  inspect="$(grep '^INSPECT' "${out}" | head -1 | mask)"
  pidfile="$(echo "${inspect}" | sed -E 's/^INSPECT pidfile=(.*) admin=([^ ]*) app_port=([^ ]*) admin_port=([^ ]*)$/\1/')"
  admin="$(echo "${inspect}" | sed -E 's/^INSPECT pidfile=(.*) admin=([^ ]*) app_port=([^ ]*) admin_port=([^ ]*)$/\2/')"
  app="$(echo "${inspect}" | sed -E 's/^INSPECT pidfile=(.*) admin=([^ ]*) app_port=([^ ]*) admin_port=([^ ]*)$/\3/')"
  adm="$(echo "${inspect}" | sed -E 's/^INSPECT pidfile=(.*) admin=([^ ]*) app_port=([^ ]*) admin_port=([^ ]*)$/\4/')"
  decision="$(grep '^DECISION' "${out}" | head -1 | sed 's/^DECISION //' | mask)"
  total_ms="$(perl -e "printf '%d', (${t1} - ${t0}) * 1000")"

  if [ -n "${BYSTANDER}" ]; then
    if is_alive "${BYSTANDER}"; then bystander_state="untouched, alive"; else bystander_state="KILLED"; fi
  else
    bystander_state="n/a"
  fi
  if [ "${action}" = adopt ] && grep -q '^WATCH' "${out}"; then
    orphan_state="orphan kept: alive in $(grep -c 'alive=true' "${out}") of $(grep -c '^WATCH' "${out}") checks"
  elif [ -n "${ORPHAN:-}" ] && { [ "${id}" = R1 ] || [ "${id}" = R2 ] || [ "${id}" = R7 ] || [ "${id}" = R7i ]; }; then
    case "$(state_of "${ORPHAN}")" in dead*) orphan_state="orphan gone" ;; *) orphan_state="orphan still running" ;; esac
  else
    orphan_state="-"
  fi
  local result="exit ${rc}"
  grep -q '^READY' "${out}" && result="${result}, new Caddy ready"
  grep -q '^ADOPTED' "${out}" && result="${result}, orphan adopted"
  grep -q '^CONFLICT' "${out}" && result="${result}, $(grep '^CONFLICT' "${out}" | head -1 | mask)"
  grep -q 'ADOPT_FAILED' "${out}" && result="${result}, adopt failed"
  grep -q '^REFUSED' "${out}" && result="${result}, $(grep -c '^REFUSED' "${out}") signal(s) refused"
  local served
  served="$(grep '^STEP.* serving ' "${out}" | head -1 | sed -E 's/.*body=//')"
  [ -n "${served}" ] && result="${result}, served ${served}"

  # Table A: what each detection method said. Table B: what the action did.
  echo "| ${id} | ${desc} | ${pidfile} | ${admin} | ${app} / ${adm} | ${decision} |" >>"${DETECT_ROWS}"
  echo "| ${id} | ${action} | $(steps_of "${out}") | ${result} | ${orphan_state}; bystander: ${bystander_state} | ${total_ms} ms |" >>"${ACTION_ROWS}"

  mkdir -p "${RESULTS}/raw"
  cp "${out}" "${RESULTS}/raw/recover-${id}-${action}.log"
  stop_pids ${TRACK}
  # The bystander of R4 is a plain \`sleep\`, which stop_pids refuses to touch.
  if [ "${id}" = R4 ] && [ -n "${BYSTANDER}" ]; then kill "${BYSTANDER}" 2>/dev/null; fi
  if ! wait_ports_free 12; then echo "ports still busy after ${id}/${action}; stopping" >&2; exit 2; fi
}

recovery() {
  local results="${RESULTS}/recovery.md"
  DETECT_ROWS="${RESULTS}/.detect.rows"
  ACTION_ROWS="${RESULTS}/.action.rows"
  : >"${DETECT_ROWS}"; : >"${ACTION_ROWS}"
  mkdir -p "${ROOT}/results"
  local id action
  for id in R1 R2 R3 R4 R5 R5b R5c R5d R5e R6 R7 R7i; do
    echo "case ${id} reap" >&2
    run_case "${id}" reap
  done
  for id in R1 R2 R7; do
    echo "case ${id} adopt" >&2
    run_case "${id}" adopt
  done
  {
    echo "# Recovery (${PLATFORM}, Caddy $("${CADDY}" version | cut -d' ' -f1))"
    echo
    echo "## What each detection method says, and what the pure \`decide\` function returns"
    echo
    echo "| Case | Setup | pid file (name + start time) | admin API (\`/id/marsa-srv\`) | port bind + \`lsof\` (site / admin) | Decision |"
    echo "| --- | --- | --- | --- | --- | --- |"
    cat "${DETECT_ROWS}"
    echo
    echo "## What each action did"
    echo
    echo "Times are milliseconds since the start of \`s06 recover\`. \`hold\` is 1 s, so the total includes it."
    echo
    echo "| Case | Action | Steps | Result | State afterwards | Wall time |"
    echo "| --- | --- | --- | --- | --- | --- |"
    cat "${ACTION_ROWS}"
  } >"${results}"
  rm -f "${DETECT_ROWS}" "${ACTION_ROWS}"
  echo "wrote ${results}" >&2
}

# --- gap ---------------------------------------------------------------------

median() { sort -n | awk '{a[NR]=$1} END {if (NR==0) print "n/a"; else if (NR%2) print a[(NR+1)/2]; else print (a[NR/2]+a[NR/2+1])/2}'; }
maxof() { sort -n | tail -1; }

gap_run() { # $1 strategy; prints "gap_ms failed ready_ms detect_ms"
  local strategy="$1" probe="${WORK}/probe.out" rec="${WORK}/recover.out"
  fresh_work
  TRACK=""
  make_orphan
  "${S06}" probe --duration-secs 5 >"${probe}" 2>&1 &
  local prb=$!
  track "${prb}"
  /bin/sleep 1
  "${S06}" recover --action "${strategy}" --hold-secs 6 >"${rec}" 2>&1
  { wait "${prb}"; } 2>/dev/null
  local gap failed ready load
  gap="$(field "${probe}" longest_gap_ms)"
  failed="$(field "${probe}" failed)"
  ready="$(grep '^STEP.*new_caddy_ready' "${rec}" | sed -E 's/^STEP t_ms=([0-9]+).*/\1/')"
  load="$(grep '^STEP.* load ' "${rec}" | sed -E 's/^STEP t_ms=([0-9]+).*/\1/')"
  local detect
  detect="$(grep '^STEP.* decided' "${rec}" | sed -E 's/^STEP t_ms=([0-9]+).*/\1/')"
  echo "${gap} ${failed} ${ready:-${load:-?}} ${detect} $(grep '^TIMING' "${rec}" | head -1)"
  stop_pids ${TRACK}
  wait_ports_free 12 || { echo "ports busy after gap run" >&2; exit 2; }
}

gap() {
  local results="${RESULTS}/gap.md" strategy i line
  mkdir -p "${ROOT}/results"
  {
    echo "# Request gap during recovery (${PLATFORM}, Caddy $("${CADDY}" version | cut -d' ' -f1))"
    echo
    echo "A probe sends one HTTP request every 25 ms to the site (300 ms timeout) while \`s06 recover\`"
    echo "handles an orphan (case R1). The gap is the time from the last good answer before the failures"
    echo "to the first good answer after them, so it can be up to one probe interval longer than the real outage."
    echo
  } >"${results}"
  local all_detect=""
  for strategy in reap adopt; do
    local gaps="" fails="" readys="" detects=""
    {
      echo "## ${strategy}, 5 runs"
      echo
      echo "| Run | gap (ms) | failed requests | ${strategy} done at (ms) | decided at (ms) |"
      echo "| --- | --- | --- | --- | --- |"
    } >>"${results}"
    for i in 1 2 3 4 5; do
      echo "gap ${strategy} run ${i}" >&2
      line="$(gap_run "${strategy}")"
      set -- ${line}
      echo "| ${i} | $1 | $2 | $3 | $4 |" >>"${results}"
      gaps="${gaps}$1"$'\n'; fails="${fails}$2"$'\n'; readys="${readys}$3"$'\n'; detects="${detects}$4"$'\n'
      all_detect="${all_detect}${line#* * * * }"$'\n'
    done
    {
      echo
      echo "- median gap: $(printf '%s' "${gaps}" | median) ms, max gap: $(printf '%s' "${gaps}" | maxof) ms"
      echo "- median failed requests: $(printf '%s' "${fails}" | median), max: $(printf '%s' "${fails}" | maxof)"
      echo "- median time to finish: $(printf '%s' "${readys}" | median) ms; median time to decide: $(printf '%s' "${detects}" | median) ms"
      echo
    } >>"${results}"
  done
  {
    echo "## Detection time per method (all 10 runs above)"
    echo
    echo "| Method | median (ms) | max (ms) |"
    echo "| --- | --- | --- |"
    local m
    for m in pidfile admin port; do
      local vals
      vals="$(printf '%s' "${all_detect}" | grep -o "${m}_ms=[0-9.]*" | cut -d= -f2)"
      echo "| ${m} | $(printf '%s\n' "${vals}" | median) | $(printf '%s\n' "${vals}" | maxof) |"
    done
  } >>"${results}"
  echo "wrote ${results}" >&2
}

# --- P5: Caddy's own pid file ---------------------------------------------------

pidfile_cmp() {
  local out="${WORK}/parent.out" pp cp
  fresh_work
  echo "# P5: Caddy's \`--pidfile\` against ours"
  "${S06}" parent --group own --stdio file --kill-on-drop off --pidfile caddy --die none >"${out}" 2>&1 &
  pp=$!
  wait_for_line "${out}" READY 15 || { echo "start failed"; return 1; }
  cp="$(field "${out}" caddy)"
  echo "- Caddy's file exists at READY: $([ -f "${WORK}/run/caddy-own.pid" ] && echo yes || echo no); content: \`$(cat "${WORK}/run/caddy-own.pid" 2>/dev/null)\` (just the pid, no path, no start time)"
  kill -9 "${pp}"; { wait "${pp}"; } 2>/dev/null
  /bin/sleep 0.5
  echo "- after SIGKILL of the parent: file $([ -f "${WORK}/run/caddy-own.pid" ] && echo kept || echo removed); Caddy $(state_of "${cp}")"
  kill -TERM "${cp}"; /bin/sleep 1
  echo "- after SIGTERM of Caddy (clean exit): file $([ -f "${WORK}/run/caddy-own.pid" ] && echo kept || echo removed)"
  wait_ports_free 10

  fresh_work
  "${S06}" parent --group own --stdio file --kill-on-drop off --pidfile caddy --die none >"${out}" 2>&1 &
  pp=$!
  wait_for_line "${out}" READY 15 || { echo "start failed"; return 1; }
  cp="$(field "${out}" caddy)"
  kill -9 "${pp}"; { wait "${pp}"; } 2>/dev/null
  kill -9 "${cp}"; /bin/sleep 0.5
  echo "- after SIGKILL of Caddy: file $([ -f "${WORK}/run/caddy-own.pid" ] && echo kept '(stale)' || echo removed)"

  # How long after spawn does Caddy's own file appear, against ours?
  fresh_work
  "${S06}" parent --group own --stdio file --kill-on-drop on --pidfile ours --die none >"${out}" 2>&1 &
  pp=$!
  wait_for_line "${out}" READY 15 || { echo "start failed"; return 1; }
  cp="$(field "${out}" caddy)"
  echo "- our file is on disk $(grep -o 'delay_us=[0-9]*' "${out}" | head -1 | cut -d= -f2) microseconds after \`spawn\` returned (includes one \`ps\` call for the start time)"
  stop_pids "${cp}" "${pp}"
  wait_ports_free 10
  local probe="${WORK}/timing.out" i
  for i in 1 2 3; do
    rm -f "${WORK}/run/caddy-own.pid"
    XDG_CONFIG_HOME="${WORK}/xdg-config" XDG_DATA_HOME="${WORK}/xdg-data" \
      perl -MTime::HiRes=time,sleep -e '
        my ($caddy, $cfg, $pf) = @ARGV; my $t = time; my $pid = fork();
        if ($pid == 0) { exec($caddy, "run", "--config", $cfg, "--pidfile", $pf) or die "exec: $!" }
        until (-e $pf) { sleep(0.001); last if time - $t > 10 }
        printf("PIDFILE_MS=%d PID=%d\n", (time - $t) * 1000, $pid);
      ' "${CADDY}" "${WORK}/caddy.json" "${WORK}/run/caddy-own.pid" >"${probe}" 2>&1
    cp="$(field "${probe}" PID)"
    echo "- Caddy's own file appeared $(field "${probe}" PIDFILE_MS) ms after \`exec\` (run ${i})"
    stop_pids "${cp}"
    wait_ports_free 10
  done
}

preflight
case "${MODE}" in
  recovery) recovery ;;
  gap) gap ;;
  pidfile) pidfile_cmp ;;
  *) echo "usage: $0 recovery|gap|pidfile" >&2; exit 1 ;;
esac
left="$(leftovers)"
if [ -n "${left}" ]; then echo "LEFTOVERS:"; echo "${left}"; else echo "no leftovers" >&2; fi
