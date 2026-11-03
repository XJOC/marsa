#!/usr/bin/env bash
# S-03 step 6: the three decision criteria, on PHP 8.4 / macOS arm64.
#   1. php -m lists Xdebug under [Zend Modules] (cli and fpm).
#   2. A real DBGp session works: init packet reaches 127.0.0.1:9003, a breakpoint hits,
#      and the final run ends with status="stopping".
#   3. One ini file switches Xdebug on and off; the php binary's sha256 does not change.
# Needs 10-fetch.sh and 30-build-xdebug-so.sh first. Uses absolute paths to the spike's own php
# (the php on PATH is Laravel Herd's and must not be used).
# Run:  ./40-check.sh        Output: results/*.txt and a PASS/FAIL summary (exit 1 on FAIL)
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

PHP="$WORK/php-cli/php"
FPM="$WORK/php-fpm/php-fpm"
SO="$WORK/out/xdebug-${PHP_VERSION%.*}-macos-arm64.so"
PORT=9003
fail=0

if lsof -nP -iTCP:$PORT -sTCP:LISTEN >/dev/null 2>&1; then echo "port $PORT is busy" >&2; exit 4; fi

# --- 1. php -m / php -v --------------------------------------------------------------
"$PHP" -n -d zend_extension="$SO" -m > "$RESULTS/php-m-cli.txt"
"$PHP" -n -d zend_extension="$SO" -v > "$RESULTS/php-v-cli.txt"
"$FPM" -n -d zend_extension="$SO" -m > "$WORK/php-m-fpm.txt"; sed -n '/Zend Modules/,$p' "$WORK/php-m-fpm.txt" > "$RESULTS/php-m-fpm-zend-modules.txt"
"$FPM" -n -d zend_extension="$SO" -v > "$RESULTS/php-v-fpm.txt"
c1=PASS
for f in "$RESULTS/php-m-cli.txt" "$RESULTS/php-m-fpm-zend-modules.txt"; do
  sed -n '/\[Zend Modules\]/,$p' "$f" | grep -qx 'Xdebug' || c1=FAIL
done
grep -q 'Xdebug' "$RESULTS/php-v-cli.txt" && grep -q 'OPcache' "$RESULTS/php-v-cli.txt" || c1=FAIL

# --- 2. DBGp session -----------------------------------------------------------------
run_session() {   # run_session OUTFILE [extra php -d args...]
  local out="$1"; shift
  "$PHP" -n "$S03_DIR/dbgp-listen.php" $PORT 5 > "$out" 2> "$out.err" &
  local lpid=$!
  trap 'kill $lpid 2>/dev/null || true' EXIT
  for _ in $(seq 1 50); do grep -q listening "$out.err" 2>/dev/null && break; sleep 0.1; done
  "$PHP" -n -d zend_extension="$SO" -d xdebug.mode=debug -d xdebug.start_with_request=yes \
      -d xdebug.client_host=127.0.0.1 -d xdebug.client_port=$PORT "$@" "$S03_DIR/hello.php" > "$out.php-stdout" 2> "$out.php-stderr" || true
  local rc=0; wait $lpid || rc=$?
  trap - EXIT
  return $rc
}
c2=PASS
run_session "$RESULTS/dbgp-session.txt" || c2=FAIL
c2b=PASS
run_session "$RESULTS/dbgp-session-opcache.txt" -d opcache.enable_cli=1 || c2b=FAIL
rm -f "$RESULTS"/dbgp-session*.err "$RESULTS"/dbgp-session*.php-std*   # keep the listener transcript only

# --- 3. ini switch -------------------------------------------------------------------
T="$WORK/ini-test"; rm -rf "$T"; mkdir -p "$T/conf.d"; : > "$T/php.ini"
before="$(shasum -a 256 "$PHP" | awk '{print $1}')"
INI="$T/conf.d/20-xdebug.ini"
run_php() { PHPRC="$T" PHP_INI_SCAN_DIR="$T/conf.d" "$PHP" "$@"; }
{
  echo "php sha256 before: $before"
  echo "== ini file ABSENT"
  run_php -m | sed -n '/Zend Modules/,$p' | tr '\n' ' '; echo
  printf 'zend_extension=%s\nxdebug.mode=debug\n' "$SO" > "$INI"
  echo "== ini file PRESENT (zend_extension + xdebug.mode=debug)"
  run_php -m | sed -n '/Zend Modules/,$p' | tr '\n' ' '; echo
  run_php -r 'echo "xdebug.mode=", ini_get("xdebug.mode"), "\n";'
  printf 'zend_extension=%s\nxdebug.mode=off\n' "$SO" > "$INI"
  echo "== ini file with xdebug.mode=off (still loaded)"
  run_php -r 'echo "extension_loaded=", var_export(extension_loaded("xdebug"), true), " mode=", ini_get("xdebug.mode"), " xdebug_info(mode)=", json_encode(xdebug_info("mode")), "\n";'
  echo "== fpm, same ini"
  PHPRC="$T" PHP_INI_SCAN_DIR="$T/conf.d" "$FPM" -m | sed -n '/Zend Modules/,$p' | tr '\n' ' '; echo
  rm -f "$INI"
  echo "== ini file ABSENT again"
  run_php -m | sed -n '/Zend Modules/,$p' | tr '\n' ' '; echo
  after="$(shasum -a 256 "$PHP" | awk '{print $1}')"
  echo "php sha256 after:  $after"
  [ "$before" = "$after" ] && echo "binary unchanged: yes" || echo "binary unchanged: NO"
} > "$RESULTS/ini-switch.txt" 2>&1
c3=PASS
grep -q 'binary unchanged: yes' "$RESULTS/ini-switch.txt" || c3=FAIL
# first ABSENT block has no Xdebug; PRESENT block has it; last ABSENT block has none
awk '/ini file ABSENT/{getline l; if (l ~ /Xdebug/) bad=1} /PRESENT/{getline l; if (l !~ /Xdebug/) bad=1} END{exit bad}' "$RESULTS/ini-switch.txt" || c3=FAIL

{
  echo "criterion 1 (php -m shows Xdebug, cli+fpm, OPcache coexists): $c1"
  echo "criterion 2 (DBGp session: init, breakpoint, stopping):        $c2"
  echo "extra: same session with opcache.enable_cli=1:                 $c2b"
  echo "criterion 3 (ini switch, binary sha256 unchanged):             $c3"
} | tee "$RESULTS/check-summary.txt"
[ "$c1$c2$c3" = "PASSPASSPASS" ] || fail=1
exit $fail
