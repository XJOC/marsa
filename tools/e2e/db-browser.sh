#!/usr/bin/env bash
# The database browser's daemon budget (ADR 0028, section 9), on a runner:
#
# 1. marsad idle, the browser never opened: resident memory 60 s after the
#    start, the median of 3 runs, at this change and at the baseline. Pass:
#    within 2 MB.
# 2. marsad while browsing a site's SQLite file of 100,000 rows and a table
#    of 1 MiB values: 200 pages, one whole value, a 1,000-row statement,
#    then `close` and 30 s. One visit, then nine more.
#    - Live heap after close (macOS): the bytes the malloc zone still has
#      allocated 30 s after the first visit's `close`, within 1 MB of
#      before the first request.
#    - Growth across visits (macOS and Linux, the default allocator):
#      resident memory after visit 10 minus after visit 2, within 2 MB.
#    The memory after the first `close`, and the peak, are recorded only:
#    they hold the freed pages the allocator keeps for reuse (ADR 0028,
#    section 9).
#
# Every run uses a new data folder; nothing touches the runner's system.
# Needs: MARSAD_NEW, MARSAD_BASE, MARSA (the CLI at this change), python3,
# jq, curl. Linux and macOS (`ps` reads another program's memory there).
set -euo pipefail

if [ "${CI:-}" != "true" ]; then
  echo "This script starts marsad many times. It runs only with CI=true (a runner)." >&2
  exit 2
fi

REPO=$(cd "$(dirname "$0")/../.." && pwd)
RUNNER_TEMP=${RUNNER_TEMP:?the runner sets RUNNER_TEMP}
NEW=${MARSAD_NEW:?the marsad of this change}
BASE=${MARSAD_BASE:?the marsad of the baseline}
CLI=${MARSA:?the marsa CLI of this change}
IDLE_LIMIT_KIB=$((2 * 1024))
LIVE_LIMIT_KIB=1024
VISITS_LIMIT_KIB=$((2 * 1024))

step() { printf '\n\n===== %s =====\n' "$*"; }
fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# The resident memory of process $1, in KiB.
rss() {
  case "$(uname -s)" in
    Linux) awk '/^VmRSS:/ { print $2 }' "/proc/$1/status" ;;
    *) ps -o rss= -p "$1" | tr -d ' ' ;;
  esac
}

# start BIN DATA: marsad in the background, once its API answers. Sets PID.
start() {
  "$1" --data-dir "$2" >"$2.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 100); do
    [ -f "$2/run/daemon.json" ] && return 0
    sleep 0.1
  done
  cat "$2.log" >&2
  fail "marsad did not start"
}

stop() {
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
}

# The median of three numbers.
median() { printf '%s\n' "$@" | sort -n | sed -n 2p; }

# idle BIN: the median of 3 runs of BIN, 60 s after each start.
idle() {
  local runs=()
  for n in 1 2 3; do
    local data=$RUNNER_TEMP/db-idle-$n-$RANDOM
    start "$1" "$data"
    sleep 60
    runs+=("$(rss "$PID")")
    stop
  done
  echo "  runs: ${runs[*]} KiB" >&2
  median "${runs[@]}"
}

step "marsad idle, the browser never opened (median of 3, 60 s after the start)"
base_idle=$(idle "$BASE")
new_idle=$(idle "$NEW")
idle_growth=$((new_idle - base_idle))
echo "idle: $new_idle KiB at this change, $base_idle KiB at the baseline: ${idle_growth} KiB (limit ±$IDLE_LIMIT_KIB)"

step "a Laravel site with a SQLite file: 100,000 rows, and 1 MiB values"
SITE=$RUNNER_TEMP/blog
mkdir -p "$SITE/public" "$SITE/database"
touch "$SITE/artisan" "$SITE/public/index.php"
python3 - "$SITE/database/database.sqlite" <<'EOF'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("create table big (id integer primary key, name text, payload text)")
db.executemany(
    "insert into big values (?, ?, ?)",
    ((n, f"row {n}", "x" * 200) for n in range(1, 100_001)),
)
db.execute("create table wide (id integer primary key, value text)")
db.executemany("insert into wide values (?, ?)", ((n, "y" * 1_048_576) for n in range(1, 4)))
db.commit()
EOF

DATA=$RUNNER_TEMP/db-browse
# shellcheck source=api.sh
. "$REPO/tools/e2e/api.sh"
start "$NEW" "$DATA"
(cd "$SITE" && "$CLI" --data-dir "$DATA" link blog >/dev/null)
sleep 5
before=$(rss "$PID")
peak=$before
sample() {
  local now
  now=$(rss "$PID")
  [ "$now" -gt "$peak" ] && peak=$now
  return 0
}

# The threads of process $1.
threads() {
  case "$(uname -s)" in
    Linux) ls "/proc/$1/task" | wc -l | tr -d ' ' ;;
    *) ps -M -p "$1" | tail -n +2 | wc -l | tr -d ' ' ;;
  esac
}

# heap: where marsad's resident memory is. On Linux, anonymous memory
# (the heap, the stacks) against file pages (the program's code). On macOS,
# the malloc zone: the bytes still allocated against the pages it holds;
# the difference is freed memory the allocator keeps for reuse.
heap() {
  case "$(uname -s)" in
    Linux) awk '/^(RssAnon|RssFile):/ { printf "      %s %s KiB\n", $1, $2 }' "/proc/$PID/status" ;;
    *)
      command -v vmmap >/dev/null || return 0
      vmmap --summary "$PID" 2>/dev/null | awk '
        /^MALLOC ZONE/ { zones = 1; next }
        zones && /^DefaultMallocZone/ {
          printf "      malloc zone: %s resident, %s allocated, %s free in the zone\n", $3, $7, $8
        }'
      ;;
  esac
}

# live: the KiB the malloc zones of marsad still have allocated (macOS).
live() {
  vmmap --summary "$PID" 2>/dev/null | awk '
    function kib(v) {
      if (v ~ /G$/) return v * 1024 * 1024
      if (v ~ /M$/) return v * 1024
      if (v ~ /K$/) return v + 0
      return v / 1024
    }
    /^MALLOC ZONE/ { zones = 1; next }
    zones && /^TOTAL/ { exit }
    zones && /Zone/ { sum += kib($7) }
    END { printf "%d\n", sum }'
}

# at NAME: marsad's memory and threads now, to find where memory stays.
at() {
  sample
  printf '  %-26s %7s KiB  %3s threads\n' "$1" "$(rss "$PID")" "$(threads "$PID")"
  case "$1" in "before open" | "after close + 30 s") heap ;; esac
}

SOURCE='{"kind":"site_file","site":"blog"}'

# browse: one visit of the window: the sources and the tables, 200 pages,
# one whole value, a statement of 1,000 rows, then close and 30 s.
browse() {
  at "before open"
  api_ok GET /v1/db/sources >/dev/null
  api_ok POST /v1/db/tables "$(jq -cn --argjson source "$SOURCE" '{source: $source}')" >/dev/null
  at "after open"
  local page body rows id kept
  for page in $(seq 0 199); do
    body=$(jq -cn --argjson source "$SOURCE" --argjson offset $((page * 500)) \
      '{source: $source, table: {name: "big"}, offset: $offset}')
    rows=$(api_ok POST /v1/db/rows "$body" | jq '.rows | length')
    [ "$rows" = 500 ] || fail "page $page has $rows rows"
    if [ $((page % 10)) = 0 ]; then sample; fi
  done
  body=$(jq -cn --argjson source "$SOURCE" \
    '{source: $source, table: {name: "wide"}, column: "value", key: ["1"]}')
  api_ok POST /v1/db/cell "$body" >/dev/null
  at "after browse"
  id=$(api_query "$SOURCE" "select * from big limit 1000")
  [ "$(api_job_end "$id" | jq -r .status)" = done ] || fail "the statement did not end done"
  kept=$(api_ok GET "/v1/db/results/$id" | jq '.rows | length')
  [ "$kept" = 1000 ] || fail "the statement kept $kept rows"
  at "after SQL"
  api_ok POST /v1/db/close >/dev/null
  at "after close"
  sleep 15
  at "after close + 15 s"
  sleep 15
  at "after close + 30 s"
}

darwin=false
if [ "$(uname -s)" = Darwin ]; then
  darwin=true
  command -v vmmap >/dev/null || fail "vmmap is needed to read the malloc zone"
  live_before=$(live)
fi

step "visit 1: 200 pages, one whole value, and a statement of 1,000 rows; close, then 30 s"
browse
after=$(rss "$PID")
close_growth=$((after - before))
echo "visit 1: $before KiB before the first request, peak $peak KiB, $after KiB 30 s after close: ${close_growth} KiB (recorded)"
if $darwin; then
  live_after=$(live)
  live_growth=$((live_after - live_before))
  echo "live heap: $live_before KiB before the first request, $live_after KiB 30 s after close: ${live_growth} KiB (limit $LIVE_LIMIT_KIB)"
fi

step "visits 2 to 10"
for visit in $(seq 2 10); do
  echo "visit $visit:"
  browse
  if [ "$visit" = 2 ]; then after_2=$(rss "$PID"); fi
done
after_10=$(rss "$PID")
visits_growth=$((after_10 - after_2))
echo "visits: $after_2 KiB after visit 2, $after_10 KiB after visit 10: ${visits_growth} KiB (limit $VISITS_LIMIT_KIB)"
stop

step "the budget"
abs() { local n=$1; echo "${n#-}"; }
failed=0
if [ "$(abs "$idle_growth")" -gt "$IDLE_LIMIT_KIB" ]; then
  echo "FAIL: idle memory moved by $idle_growth KiB"
  failed=1
fi
if $darwin && [ "$live_growth" -gt "$LIVE_LIMIT_KIB" ]; then
  echo "FAIL: after close the malloc zone still holds $live_growth KiB more than before"
  failed=1
fi
if [ "$visits_growth" -gt "$VISITS_LIMIT_KIB" ]; then
  echo "FAIL: visits 3 to 10 added $visits_growth KiB"
  failed=1
fi
live_note=""
if $darwin; then live_note=", live heap ${live_growth} KiB"; fi
echo "db-browser budget ($(uname -sm)): idle ${idle_growth} KiB, peak $peak KiB, after close ${close_growth} KiB (recorded)${live_note}, visits 2 to 10 ${visits_growth} KiB"
exit "$failed"
