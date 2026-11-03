#!/usr/bin/env bash
# S-09 measurement (macOS only): Tauri vs egui. See README.md.
#
# Builds the three programs in release mode, runs cases A..E with `--bench`,
# samples memory with `footprint` at every `PHASE` line, measures idle CPU
# from cumulative CPU time, and prints one table of medians.
#
# Written for the bash 3.2 that ships with macOS: no associative arrays.
# Needs no sudo: `footprint` and `ps` can read processes of the same user,
# and the WebKit processes of the app run as that user.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
TARGET="$ROOT/target/release"
TRAY_BIN="$TARGET/tray"
EGUI_BIN="$TARGET/egui-app"
TAURI_APP="$TARGET/bundle/macos/tauri-app.app"
TAURI_BIN="$TAURI_APP/Contents/MacOS/tauri-app"
RESPONSIBLE_JS="$ROOT/responsible-pids.js"

RUNS=3
WARMUP=1
CASES=ABCDE
BUILD=1
# Longest silence between two PHASE lines: idle (10 s) plus a pause (5 s), or
# a cold first start. Anything longer means the program is stuck.
LINE_TIMEOUT=90
# Seconds between runs, so the previous app and its WebKit processes are gone.
COOLDOWN=3

usage() {
  cat <<'USAGE'
usage: ./measure.sh [options]
  --runs N       measured runs per case (default 3; the table shows medians)
  --warmup N     unmeasured warm-up runs per case, before the measured ones
                 (default 1: the first start after a build is a cold start)
  --quick        one run, no warm-up (same as --runs 1 --warmup 0)
  --cases LIST   cases to run, for example B or ABD (default ABCDE)
  --no-build     use the programs already built in target/release
  -h, --help     show this help

cases:
  A  tray alone, no window          tray --bench
  B  egui, glow renderer            egui-app --renderer glow --bench
  C  egui, wgpu renderer            egui-app --renderer wgpu --bench
  D  Tauri                          tauri-app --bench
  E  Tauri with tray, window closed tauri-app --with-tray --bench
USAGE
}

die() {
  echo "measure.sh: $*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --runs) RUNS="${2:-}"; shift 2 || die "--runs needs a number" ;;
    --warmup) WARMUP="${2:-}"; shift 2 || die "--warmup needs a number" ;;
    --quick) RUNS=1; WARMUP=0; shift ;;
    --cases) CASES="${2:-}"; shift 2 || die "--cases needs a list" ;;
    --no-build) BUILD=0; shift ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

case "$RUNS" in '' | *[!0-9]*) die "--runs must be a number" ;; esac
case "$WARMUP" in '' | *[!0-9]*) die "--warmup must be a number" ;; esac
[ "$RUNS" -ge 1 ] || die "--runs must be 1 or more"
case "$CASES" in '' | *[!A-E]*) die "--cases takes letters A to E, for example ABD" ;; esac
[ "$(uname -s)" = Darwin ] || die "macOS only"
command -v footprint >/dev/null || die "footprint not found (it ships with macOS)"

# ---------------------------------------------------------------- build

if [ "$BUILD" -eq 1 ]; then
  echo "==> build (release profile in Cargo.toml: opt-level 3, fat LTO, 1 codegen unit, strip)"
  # One package per cargo call: features are not unified across programs, so
  # each binary contains only what it needs.
  (cd "$ROOT" && cargo build --release -p tray)
  (cd "$ROOT" && cargo build --release -p egui-app)
  (cd "$ROOT/tauri-app" && pnpm install --frozen-lockfile && pnpm tauri build --bundles app)
fi
for program in "$TRAY_BIN" "$EGUI_BIN" "$TAURI_BIN"; do
  [ -x "$program" ] || die "missing $program (run without --no-build)"
done

if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
  echo "warning: cargo or rustc is running; timings will be noisy" >&2
fi

# ---------------------------------------------------------------- helpers

STAMP="$(date +%Y%m%d-%H%M%S)"
OUT="$ROOT/results/$STAMP"
mkdir -p "$OUT/logs"
RAW="$OUT/samples.tsv"
MEDIANS="$OUT/medians.tsv"
TABLE="$OUT/table.md"
FIFO="$OUT/phase.fifo"
printf 'case\trun\tphase\tmetric\tvalue\n' >"$RAW"

APP_PID=""
BASELINE=""
cleanup() {
  if [ -n "$APP_PID" ]; then kill "$APP_PID" 2>/dev/null || true; fi
  rm -f "$FIFO" "$OUT/cpu-start.txt" "$OUT/cpu-end.txt"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

now() { perl -MTime::HiRes=time -e 'printf("%.3f\n", time)'; }

record() { printf '%s\t%s\t%s\t%s\t%s\n' "$@" >>"$RAW"; }

mib() { awk -v bytes="$1" 'BEGIN { printf "%.1f", bytes / 1048576 }'; }

# PIDs of every WebKit XPC process on the machine.
webkit_all() { ps -axo pid=,comm= | awk '/com\.apple\.WebKit\./ { print $1 }'; }

# How WebKit processes are tied to the app: by "responsible process" when the
# JXA helper works on this macOS, otherwise only by "started after the app".
if osascript -l JavaScript "$RESPONSIBLE_JS" "$$" "$$" >/dev/null 2>&1; then
  ATTRIBUTION="responsible-pid"
else
  ATTRIBUTION="new-since-launch"
  echo "warning: responsible-PID lookup failed; using WebKit processes started after the app" >&2
fi

# WebKit processes of the app with PID $1, separated by spaces. Processes in
# the pre-launch snapshot ($BASELINE) never count.
webkit_pids() {
  local app_pid=$1 candidates="" pid
  for pid in $(webkit_all); do
    case "$BASELINE" in *" $pid "*) ;; *) candidates="$candidates $pid" ;; esac
  done
  [ -n "$candidates" ] || return 0
  if [ "$ATTRIBUTION" = responsible-pid ]; then
    # shellcheck disable=SC2086 # one argument per PID
    { osascript -l JavaScript "$RESPONSIBLE_JS" "$app_pid" $candidates 2>/dev/null || true; } | tr '\n' ' '
  else
    echo "$candidates"
  fi
}

# Every PID measured for a case: the app, plus its WebKit processes (Tauri).
case_pids() {
  local app_pid=$1 kind=$2
  if [ "$kind" = webkit ]; then
    echo "$app_pid $(webkit_pids "$app_pid")"
  else
    echo "$app_pid"
  fi
}

# Memory of the given PIDs ($1 is the app). Prints
# "total_bytes app_bytes webkit_bytes webkit_count". The value is the kernel's
# phys_footprint, the number Activity Monitor shows as "Memory".
sample_memory() {
  local app_pid=$1
  { footprint -f bytes --noCategories "$@" 2>/dev/null || true; } | awk -v app="$app_pid" '
    match($0, /\[[0-9]+\]:/) { pid = substr($0, RSTART + 1, RLENGTH - 3) }
    $1 == "phys_footprint:" { if (pid == app) a += $2; else { w += $2; n++ } }
    END { printf "%.0f %.0f %.0f %d\n", a + w, a, w, n }'
}

# Cumulative CPU seconds per PID: "pid seconds" lines.
sample_cpu() {
  local list
  list=$(echo $* | tr ' ' ',')
  { ps -o pid=,time= -p "$list" 2>/dev/null || true; } | awk '
    function seconds(text,   days, parts, count, i, total) {
      days = 0
      if (index(text, "-") > 0) { split(text, parts, "-"); days = parts[1]; text = parts[2] }
      count = split(text, parts, ":")
      total = 0
      for (i = 1; i <= count; i++) total = total * 60 + parts[i]
      return days * 86400 + total
    }
    { print $1, seconds($2) }'
}

# CPU percent of one core between two samples: $1 start file, $2 end file,
# $3 wall seconds. A PID that appeared in between counts with all its time.
cpu_percent() {
  awk -v wall="$3" '
    NR == FNR { start[$1] = $2; next }
    { used = ($1 in start) ? $2 - start[$1] : $2; if (used > 0) total += used }
    END { printf "%.2f\n", (wall > 0) ? 100 * total / wall : 0 }' "$1" "$2"
}

# Waits up to 20 s for PID $1 to exit, then stops it.
wait_exit() {
  local pid=$1 ticks=0
  while kill -0 "$pid" 2>/dev/null; do
    ticks=$((ticks + 1))
    if [ "$ticks" -eq 200 ]; then kill "$pid" 2>/dev/null || true; fi
    if [ "$ticks" -ge 300 ]; then kill -9 "$pid" 2>/dev/null || true; break; fi
    sleep 0.1
  done
  wait "$pid" 2>/dev/null || true
}

# ---------------------------------------------------------------- one run

# run_case <case> <run label>. Returns non-zero when the run did not finish.
run_case() {
  local id=$1 run=$2 kind line rest name fields kv pids
  local total app webkit count idle_wall="" end_wall cpu done_seen=0
  local -a cmd
  case "$id" in
    A) cmd=("$TRAY_BIN" --bench); kind=native ;;
    B) cmd=("$EGUI_BIN" --renderer glow --bench); kind=native ;;
    C) cmd=("$EGUI_BIN" --renderer wgpu --bench); kind=native ;;
    D) cmd=("$TAURI_BIN" --bench); kind=webkit ;;
    E) cmd=("$TAURI_BIN" --with-tray --bench); kind=webkit ;;
  esac

  BASELINE=" $(webkit_all | tr '\n' ' ') "
  rm -f "$FIFO"
  mkfifo "$FIFO"
  "${cmd[@]}" >"$FIFO" 2>"$OUT/logs/$id-$run.stderr" &
  APP_PID=$!
  exec 3<"$FIFO"

  while IFS= read -r -t "$LINE_TIMEOUT" line <&3; do
    case "$line" in "PHASE "*) ;; *) continue ;; esac
    rest=${line#PHASE }
    name=${rest%% *}
    fields=""
    if [ "$rest" != "$name" ]; then fields=${rest#* }; fi
    for kv in $fields; do record "$id" "$run" "$name" "${kv%%=*}" "${kv#*=}"; done

    case "$name" in
      done)
        done_seen=1
        printf '  [%s %-2s] %-14s\n' "$id" "$run" "$name"
        break
        ;;
      idle-start)
        pids=$(case_pids "$APP_PID" "$kind")
        idle_wall=$(now)
        # shellcheck disable=SC2086 # one argument per PID
        sample_cpu $pids >"$OUT/cpu-start.txt"
        printf '  [%s %-2s] %-14s\n' "$id" "$run" "$name"
        continue
        ;;
    esac

    pids=$(case_pids "$APP_PID" "$kind")
    if [ "$name" = idle ] && [ -n "$idle_wall" ]; then
      end_wall=$(now)
      # shellcheck disable=SC2086 # one argument per PID
      sample_cpu $pids >"$OUT/cpu-end.txt"
      cpu=$(cpu_percent "$OUT/cpu-start.txt" "$OUT/cpu-end.txt" \
        "$(awk -v a="$idle_wall" -v b="$end_wall" 'BEGIN { print b - a }')")
      record "$id" "$run" idle idle_cpu_pct "$cpu"
      fields="$fields cpu=${cpu}%"
    fi
    # shellcheck disable=SC2086 # one argument per PID
    read -r total app webkit count <<<"$(sample_memory $pids)"
    record "$id" "$run" "$name" mem_total_mib "$(mib "$total")"
    record "$id" "$run" "$name" mem_app_mib "$(mib "$app")"
    record "$id" "$run" "$name" mem_webkit_mib "$(mib "$webkit")"
    record "$id" "$run" "$name" webkit_procs "$count"
    printf '  [%s %-2s] %-14s mem=%s MiB (webkit %s: %s) %s\n' \
      "$id" "$run" "$name" "$(mib "$total")" "$count" "$(mib "$webkit")" "$fields"
  done
  exec 3<&-

  if [ "$done_seen" -eq 0 ]; then
    echo "  [$id $run] FAILED: no 'PHASE done' (see $OUT/logs/$id-$run.stderr)" >&2
    kill "$APP_PID" 2>/dev/null || true
  fi
  wait_exit "$APP_PID"
  APP_PID=""
  rm -f "$FIFO"
  [ "$done_seen" -eq 1 ] || return 1
  record "$id" "$run" done ok 1
}

# ---------------------------------------------------------------- runs

echo "==> S-09 measure: cases $CASES, $RUNS measured run(s) + $WARMUP warm-up per case"
echo "    macOS $(sw_vers -productVersion), $(sysctl -n hw.model), WebKit attribution: $ATTRIBUTION"
echo "    Do not use the Mac during the runs: the windows float on top, and input"
echo "    or another build changes the numbers."
echo "    Results: $OUT"

for id in $(echo "$CASES" | fold -w1); do
  echo "==> case $id"
  index=1
  while [ "$index" -le $((WARMUP + RUNS)) ]; do
    if [ "$index" -le "$WARMUP" ]; then label="w$index"; else label=$((index - WARMUP)); fi
    run_case "$id" "$label" || true
    sleep "$COOLDOWN"
    index=$((index + 1))
  done
done

# ---------------------------------------------------------------- table

size_mib() { mib "$(stat -f %z "$1")"; }
SIZE_TRAY=$(size_mib "$TRAY_BIN")
SIZE_EGUI=$(size_mib "$EGUI_BIN")
SIZE_TAURI=$(size_mib "$TAURI_BIN")
SIZE_TAURI_APP=$(awk -v kib="$(du -sk "$TAURI_APP" | awk '{ print $1 }')" 'BEGIN { printf "%.1f", kib / 1024 }')

# Median per (case, phase, metric) over the measured runs (warm-ups skipped).
awk -F '\t' '
  NR == 1 || $2 ~ /^w/ { next }
  { key = $1 FS $3 FS $4; values[key] = (key in values) ? values[key] " " $5 : $5 }
  END {
    for (key in values) {
      count = split(values[key], list, " ")
      for (i = 2; i <= count; i++) {
        value = list[i] + 0
        for (j = i - 1; j >= 1 && list[j] + 0 > value; j--) list[j + 1] = list[j]
        list[j + 1] = value
      }
      median = (count % 2) ? list[(count + 1) / 2] : (list[count / 2] + list[count / 2 + 1]) / 2
      print key FS median FS count
    }
  }' "$RAW" | sort >"$MEDIANS"

awk -F '\t' \
  -v cases="$CASES" -v runs="$RUNS" -v attribution="$ATTRIBUTION" \
  -v size_tray="$SIZE_TRAY" -v size_egui="$SIZE_EGUI" \
  -v size_tauri="$SIZE_TAURI" -v size_tauri_app="$SIZE_TAURI_APP" '
  { median[$1 SUBSEP $2 SUBSEP $3] = $4 }
  $2 == "done" && $3 == "ok" { finished[$1] = $5 }
  function m(c, phase, metric,   key) {
    key = c SUBSEP phase SUBSEP metric
    return (key in median) ? median[key] : ""
  }
  function f(value, decimals) {
    return (value == "") ? "-" : sprintf("%." decimals "f", value)
  }
  function frames(c, phase) {
    if (m(c, phase, "p95") == "") return "-"
    return f(m(c, phase, "p50"), 1) " / " f(m(c, phase, "p95"), 1) " / " f(m(c, phase, "max"), 1)
  }
  # Adds one table row; cells are padded when the table is printed.
  function add(line,   count, i) {
    count = split(line, parts, "\t")
    rows++
    for (i = 1; i <= count; i++) {
      cell[rows, i] = parts[i]
      if (length(parts[i]) > width[i]) width[i] = length(parts[i])
    }
    if (count > columns) columns = count
  }
  function row(c, program, size, start_phase,   closed) {
    closed = f(m(c, "window-closed", "mem_total_mib"), 1)
    if (closed != "-") {
      closed = closed " (WebKit " f(m(c, "window-closed", "webkit_procs"), 0) " procs: " \
        f(m(c, "window-closed", "mem_webkit_mib"), 1) ")"
    }
    add(c "\t" program "\t" ((c in finished) ? finished[c] : 0) "/" runs "\t" size "\t" \
      f(m(c, start_phase, "ms"), 0) "\t" \
      f(m(c, start_phase, "mem_total_mib"), 1) "\t" f(m(c, "table-scrolled", "mem_total_mib"), 1) "\t" \
      f(m(c, "sql-loaded", "mem_total_mib"), 1) "\t" f(m(c, "logs-scrolled", "mem_total_mib"), 1) "\t" \
      f(m(c, "idle", "mem_total_mib"), 1) "\t" closed "\t" \
      frames(c, "table-scrolled") "\t" f(m(c, "table-scrolled", "cpu_p95"), 1) "\t" \
      f(m(c, "logs-scrolled", "p95"), 1) "\t" f(m(c, "sql-loaded", "ms"), 0) "\t" \
      f(m(c, "idle", "idle_cpu_pct"), 2))
  }
  END {
    add("Case\tProgram\tRuns\tSize MiB\t1st frame ms\tMem start\tMem table\tMem SQL\tMem logs\tMem idle\tMem after close\tTable frame p50 / p95 / max ms\tTable CPU/frame p95 ms\tLogs frame p95 ms\tSQL load ms\tIdle CPU %")
    if (index(cases, "A")) row("A", "tray (icon, no window)", size_tray, "ready")
    if (index(cases, "B")) row("B", "egui-app --renderer glow", size_egui, "first-frame")
    if (index(cases, "C")) row("C", "egui-app --renderer wgpu", size_egui, "first-frame")
    if (index(cases, "D")) row("D", "tauri-app", size_tauri " (.app " size_tauri_app ")", "first-frame")
    if (index(cases, "E")) row("E", "tauri-app --with-tray", size_tauri " (.app " size_tauri_app ")", "first-frame")
    for (r = 1; r <= rows; r++) {
      line = "|"
      for (i = 1; i <= columns; i++) line = line " " sprintf("%-" width[i] "s", cell[r, i]) " |"
      print line
      if (r == 1) {
        line = "|"
        for (i = 1; i <= columns; i++) {
          dashes = ""
          for (k = 0; k < width[i]; k++) dashes = dashes "-"
          line = line " " dashes " |"
        }
        print line
      }
    }
    print ""
    print "Medians of the measured runs (Runs = runs that finished). Memory: phys_footprint from"
    print "`footprint` in MiB, sampled at each PHASE line; Tauri = app + its WebKit processes"
    print "(attribution: " attribution "). Case A: \"1st frame\" and \"Mem start\" = tray icon ready."
    print "Frame interval: wall time between consecutive frames while scrolling top to bottom in 5 s"
    print "(egui: start of each frame; Tauri: requestAnimationFrame timestamps, 1 ms resolution)."
    print "Table CPU/frame: eframe CPU time per frame (egui only). Idle CPU: CPU time during the"
    print "10 s idle phase / wall time, percent of one core. Tauri \"Mem table\" and \"Mem logs\" are"
    print "sampled right after the scroll: they include WebKit tile surfaces made while scrolling"
    print "(much of it volatile), which WebKit releases about 3 s later, before the next phase."
  }' "$MEDIANS" >"$TABLE"

echo
cat "$TABLE"
echo
echo "Raw samples: $RAW"
echo "Medians:     $MEDIANS"
