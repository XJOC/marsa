#!/usr/bin/env bash
# S-12b: build a fresh Laravel app and run the cache and queue probes against a
# key-value server that is ALREADY listening on 127.0.0.1:$PORT.
#
#   laravel/run.sh PORT WORK_DIR OUT_DIR LABEL
#
# Writes OUT_DIR/laravel-LABEL.jsonl (one result per line) and
# OUT_DIR/laravel-LABEL.log. Exit 0 even when checks fail: the results are the
# answer. Needs php and composer on PATH. Runs on GitHub runners only.
set -uo pipefail
PORT="${1:?port}"; WORK="${2:?work dir}"; OUT="${3:?out dir}"; LABEL="${4:?label}"
KV="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$WORK" "$OUT"
LOG="$OUT/laravel-$LABEL.log"
: >"$LOG"
say() { echo "[laravel] $*" | tee -a "$LOG"; }

APP="$WORK/app"
if [ ! -d "$APP" ]; then
  say "composer create-project laravel/laravel"
  (cd "$WORK" && composer create-project laravel/laravel app --no-interaction --prefer-dist --quiet) >>"$LOG" 2>&1 || { say "create-project FAILED"; exit 0; }
  (cd "$APP" && composer require predis/predis --no-interaction --quiet) >>"$LOG" 2>&1 || { say "require predis FAILED"; exit 0; }
  cp -R "$KV/laravel/files/app/." "$APP/app/"
  sed -i.bak "s/'block_for' => null/'block_for' => env('REDIS_QUEUE_BLOCK_FOR')/" "$APP/config/queue.php"
  (cd "$APP" && php artisan migrate --force) >>"$LOG" 2>&1 || say "migrate FAILED"
fi
cd "$APP" || exit 0
{
  echo "laravel=$(php artisan --version)"
  echo "php=$(php -r 'echo PHP_VERSION;')"
  echo "predis=$(composer show predis/predis 2>/dev/null | grep -E '^versions' )"
} | tee -a "$LOG"

export REDIS_CLIENT=predis CACHE_STORE=redis QUEUE_CONNECTION=redis SESSION_DRIVER=file
export REDIS_HOST=127.0.0.1 REDIS_PORT="$PORT" REDIS_PASSWORD=null DB_CONNECTION=sqlite
export REDIS_PREFIX="kv_${LABEL//[^a-zA-Z0-9]/_}_"
rm -f storage/kv-results.jsonl storage/kv-evidence.log

run() { say "\$ php artisan $*"; php artisan "$@" 2>&1 | tee -a "$LOG"; }
work() { say "\$ queue:work $*"; perl -e 'alarm 90; exec @ARGV' php artisan queue:work redis --queue=high,default --stop-when-empty --sleep=1 --max-time=60 "$@" 2>&1 | tee -a "$LOG"; echo "worker exit: ${PIPESTATUS[0]}" | tee -a "$LOG"; }

run kv:cache
run kv:dispatch basic
work
run kv:verify 1
say "waiting 5 s for the delayed and released jobs"
sleep 5
work
run kv:verify 2
REDIS_QUEUE_BLOCK_FOR=1 run kv:dispatch block
REDIS_QUEUE_BLOCK_FOR=1 work
run kv:verify block

cp storage/kv-results.jsonl "$OUT/laravel-$LABEL.jsonl" 2>/dev/null || echo '[]' >"$OUT/laravel-$LABEL.jsonl"
cp storage/logs/laravel.log "$OUT/laravel-$LABEL-app.log" 2>/dev/null || true
say "done: $(grep -c '"ok":true' "$OUT/laravel-$LABEL.jsonl") passed, $(grep -c '"ok":false' "$OUT/laravel-$LABEL.jsonl") failed"
exit 0
