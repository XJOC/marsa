#!/usr/bin/env bash
# S-10: does a brand-new Laravel project work on the PHP we built?
#
#   laravel-check.sh <php> [work dir]
#
# Downloads composer.phar, runs `composer create-project laravel/laravel` (this checks
# every ext-* Laravel needs, and its post-create scripts run artisan and migrate the
# default SQLite database), then `php artisan test`. Needs the network.
# Writes $OUT/laravel.md. Exit code 1 on failure.
#
# The PHP is started with -n (no php.ini), and a CA bundle is passed by hand because a
# static OpenSSL does not know where the system bundle is on every OS.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${OUT:-$HERE/out}"
PHP="${1:?usage: laravel-check.sh <php> [work dir]}"
DIR="${2:-$HERE/work/laravel}"

mkdir -p "$OUT" "$DIR"
: > "$OUT/laravel.md"
log() { printf '%s\n' "$*" | tee -a "$OUT/laravel.md"; }

if [ -f /etc/ssl/cert.pem ]; then
  CA=/etc/ssl/cert.pem
elif [ -f /etc/ssl/certs/ca-certificates.crt ]; then
  CA=/etc/ssl/certs/ca-certificates.crt
else
  CA=""
fi
args=(-n -d memory_limit=-1)
if [ -n "$CA" ]; then args+=(-d "openssl.cafile=$CA" -d "curl.cainfo=$CA"); fi

curl -fsSL https://getcomposer.org/download/latest-stable/composer.phar -o "$DIR/composer.phar"
log "composer: $("$PHP" "${args[@]}" "$DIR/composer.phar" --version --no-ansi 2>&1 | head -1)"

rm -rf "$DIR/app"
"$PHP" "${args[@]}" "$DIR/composer.phar" create-project laravel/laravel "$DIR/app" \
  --no-interaction --no-progress --prefer-dist --no-ansi > "$OUT/laravel-create.txt" 2>&1 || {
  tail -40 "$OUT/laravel-create.txt"
  log "FAIL composer create-project laravel/laravel"
  exit 1
}
log "PASS composer create-project laravel/laravel (all ext-* requirements met)"
log "laravel: $("$PHP" -n "$DIR/app/artisan" --version --no-ansi 2>&1 | head -1)"

cd "$DIR/app"
if "$PHP" -n -d memory_limit=-1 artisan migrate:status --no-ansi > "$OUT/laravel-migrate.txt" 2>&1; then
  log "PASS artisan migrate:status (SQLite database reachable)"
else
  tail -20 "$OUT/laravel-migrate.txt"
  log "FAIL artisan migrate:status"
  exit 1
fi
if "$PHP" -n -d memory_limit=-1 artisan test --no-ansi > "$OUT/laravel-test.txt" 2>&1; then
  log "PASS artisan test: $(grep -E 'Tests:|"result"' "$OUT/laravel-test.txt" | tail -1 | sed 's/^ *//')"
else
  tail -40 "$OUT/laravel-test.txt"
  log "FAIL artisan test"
  exit 1
fi
