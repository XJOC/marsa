#!/usr/bin/env bash
# S-03 extra: is ONE xdebug.so per PHP minor enough? Load the xdebug.so built against 8.4.23 headers
# into OLDER 8.4 patch releases (prebuilt static-php-cli "bulk" cli) and run the DBGp session on each.
# Also counts the symbols each php binary exports: a php that exports none cannot load ANY .so.
# Run:  ./60-patch-compat.sh [version ...]     (default: 8.4.1 8.4.10)     Output: results/patch-compat.txt
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

VERSIONS=("$@"); [ ${#VERSIONS[@]} -gt 0 ] || VERSIONS=(8.4.1 8.4.10)
LISTENER_PHP="$WORK/php-cli/php"
SO="$WORK/out/xdebug-${PHP_VERSION%.*}-macos-arm64.so"
PORT=9003
need_disk_gb 8
if lsof -nP -iTCP:$PORT -sTCP:LISTEN >/dev/null 2>&1; then echo "port $PORT is busy" >&2; exit 4; fi
: > "$RESULTS/patch-compat.txt"

for v in "${VERSIONS[@]}"; do
  fetch "https://dl.static-php.dev/static-php-cli/bulk/php-$v-cli-macos-aarch64.tar.gz"
  d="$WORK/php-compat/$v"; mkdir -p "$d"; [ -x "$d/php" ] || tar -xzf "$DL/php-$v-cli-macos-aarch64.tar.gz" -C "$d"
  P="$d/php"
  out="$WORK/compat-$v.txt"
  "$LISTENER_PHP" -n "$S03_DIR/dbgp-listen.php" $PORT 5 > "$out" 2> "$out.err" &
  lpid=$!
  for _ in $(seq 1 50); do grep -q listening "$out.err" 2>/dev/null && break; sleep 0.1; done
  "$P" -n -d zend_extension="$SO" -d xdebug.mode=debug -d xdebug.start_with_request=yes \
       -d xdebug.client_host=127.0.0.1 -d xdebug.client_port=$PORT "$S03_DIR/hello.php" >/dev/null 2>"$out.php-stderr" || true
  rc=0; wait $lpid || rc=$?
  {
    echo "== PHP $v ($("$P" -n -i | grep -E '^PHP API' | sed -n '1p'); built $("$P" -n -i | grep -E '^Build Date' | sed -n '1p' | sed 's/.*=> //'); exported symbols: $(nm -gU "$P" | wc -l))"
    "$P" -n -d zend_extension="$SO" -v 2>&1 | sed -n '5,6p'
    grep '^RESULT' "$out" || echo "RESULT missing"
    echo "dbgp session exit code: $rc"
    [ -s "$out.php-stderr" ] && { echo "php stderr:"; head -3 "$out.php-stderr"; }
  } >> "$RESULTS/patch-compat.txt"
done
cat "$RESULTS/patch-compat.txt"
