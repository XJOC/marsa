#!/usr/bin/env bash
# S-10 smoke tests for the PHP we built.
#
#   smoke.sh <php> <php-fpm> [xdebug.so]
#
# Environment:
#   OUT          where the logs go (default: out/)
#   DLOPEN       "true" (default) if this build must export Zend symbols and load
#                a .so; "false" for static musl (the symbol check becomes INFO)
#   CAFILE       a CA bundle for the HTTPS check in runtime-checks.php (optional)
#
# Checks: php -v, php -m against expected-modules.txt, exported zend_* symbol count
# (>= 1000), Xdebug loaded from the shared .so, runtime checks (SQLite, intl, gd, ...),
# OPcache on, and one real FastCGI request through php-fpm.
# Writes $OUT/smoke.md (a table) and $OUT/smoke-status.txt (PASS=n FAIL=n).
# Exit code 1 if any check fails.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${OUT:-$HERE/out}"
DLOPEN="${DLOPEN:-true}"
PHP="${1:?usage: smoke.sh <php> <php-fpm> [xdebug.so]}"
FPM="${2:?usage: smoke.sh <php> <php-fpm> [xdebug.so]}"
XDEBUG="${3:-}"

mkdir -p "$OUT"
MD="$OUT/smoke.md"
: > "$MD"
pass=0
fail=0

record() { # record <PASS|FAIL|INFO> <name> <detail>
  local status="$1" name="$2" detail="${3:-}"
  detail="$(printf '%s' "$detail" | tr '\n' ' ' | cut -c1-300)"
  printf '%s %s %s\n' "$status" "$name" "$detail"
  printf '| %s | %s | %s |\n' "$status" "$name" "$detail" >> "$MD"
  case "$status" in
    PASS) pass=$((pass + 1)) ;;
    FAIL) fail=$((fail + 1)) ;;
  esac
}

# Lowercase, sorted, unique module names. Drops the [PHP Modules] headers and blanks.
# php-fpm lists one extra pseudo-module, cgi-fcgi, so it is dropped too.
norm_modules() { grep -v '^\[' | grep -v '^[[:space:]]*$' | sed 's/\r$//' | tr 'A-Z' 'a-z' | grep -vx 'cgi-fcgi' | sort -u; }

{
  echo '| | check | detail |'
  echo '| --- | --- | --- |'
} >> "$MD"

# 1. Version ----------------------------------------------------------------
"$PHP" -n -v > "$OUT/php-v.txt" 2>&1
first="$(head -1 "$OUT/php-v.txt")"
if grep -q '^PHP 8\.' "$OUT/php-v.txt"; then record PASS "php -v" "$first"; else record FAIL "php -v" "$first"; fi
"$FPM" -n -v > "$OUT/fpm-v.txt" 2>&1
if grep -q '^PHP 8\.' "$OUT/fpm-v.txt"; then record PASS "php-fpm -v" "$(head -1 "$OUT/fpm-v.txt")"; else record FAIL "php-fpm -v" "$(head -1 "$OUT/fpm-v.txt")"; fi
"$PHP" -n -i 2>/dev/null | grep -E '^(PHP Version|PHP API|PHP Extension Build|Thread Safety|Debug Build|Build Provider|Build System|Configure Command|Zend Extension Build|extension_dir)' > "$OUT/php-i.txt" || true
record INFO "build provider" "$(grep -i 'Build Provider' "$OUT/php-i.txt" | head -1)"
record INFO "api / thread safety / debug" "$(grep -E '^(PHP API|Thread Safety|Debug Build)' "$OUT/php-i.txt" | tr '\n' ';')"

# 2. php -m against the expected list --------------------------------------
"$PHP" -n -m | norm_modules > "$OUT/modules-actual.txt"
grep -v '^#' "$HERE/expected-modules.txt" | norm_modules > "$OUT/modules-expected.txt"
comm -23 "$OUT/modules-expected.txt" "$OUT/modules-actual.txt" > "$OUT/modules-missing.txt"
comm -13 "$OUT/modules-expected.txt" "$OUT/modules-actual.txt" > "$OUT/modules-extra.txt"
if [ -s "$OUT/modules-missing.txt" ]; then
  record FAIL "php -m: missing modules" "$(tr '\n' ' ' < "$OUT/modules-missing.txt")"
else
  record PASS "php -m: all $(wc -l < "$OUT/modules-expected.txt" | tr -d ' ') expected modules present" ""
fi
record INFO "php -m: modules not in the list" "$(tr '\n' ' ' < "$OUT/modules-extra.txt")"
"$FPM" -n -m | norm_modules > "$OUT/fpm-modules-actual.txt"
if diff -q "$OUT/modules-actual.txt" "$OUT/fpm-modules-actual.txt" > /dev/null; then
  record PASS "php-fpm -m equals php -m" ""
else
  record FAIL "php-fpm -m equals php -m" "$(diff "$OUT/modules-actual.txt" "$OUT/fpm-modules-actual.txt" | tr '\n' ' ')"
fi

# 3. Exported Zend symbols (ADR 0010 smoke test) ------------------------------
# A .so extension needs these from the main binary. macOS: nm -gU. Linux: nm -D.
case "$(uname -s)" in
  Darwin) zsyms="$(nm -gU "$PHP" 2>/dev/null | grep -c ' _zend_' || true)" ;;
  *) zsyms="$(nm -D --defined-only "$PHP" 2>/dev/null | grep -c ' zend_' || true)" ;;
esac
zsyms="${zsyms:-0}"
if [ "$zsyms" -ge 1000 ]; then
  record PASS "exported zend_* symbols >= 1000" "$zsyms"
elif [ "$DLOPEN" = "true" ]; then
  record FAIL "exported zend_* symbols >= 1000" "$zsyms (this build cannot load a .so)"
else
  record INFO "exported zend_* symbols" "$zsyms (static build, cannot load a .so, as expected)"
fi

# 4. Xdebug from the shared .so ---------------------------------------------
if [ -n "$XDEBUG" ] && [ -f "$XDEBUG" ]; then
  XDEBUG="$(cd "$(dirname "$XDEBUG")" && pwd)/$(basename "$XDEBUG")"
  if "$PHP" -n -d "zend_extension=$XDEBUG" -m > "$OUT/xdebug-m.txt" 2>&1 && grep -qx 'Xdebug' "$OUT/xdebug-m.txt"; then
    record PASS "php -d zend_extension=xdebug.so -m lists Xdebug" ""
  else
    record FAIL "php -d zend_extension=xdebug.so -m lists Xdebug" "$(head -c 300 "$OUT/xdebug-m.txt")"
  fi
  "$PHP" -n -d "zend_extension=$XDEBUG" -v > "$OUT/xdebug-v.txt" 2>&1 || true
  if grep -q 'with Xdebug' "$OUT/xdebug-v.txt"; then record PASS "php -v shows Xdebug" "$(grep 'Xdebug' "$OUT/xdebug-v.txt" | head -1)"; else record FAIL "php -v shows Xdebug" "$(head -c 200 "$OUT/xdebug-v.txt")"; fi
  if "$FPM" -n -d "zend_extension=$XDEBUG" -m 2>&1 | grep -qx 'Xdebug'; then record PASS "php-fpm loads Xdebug" ""; else record FAIL "php-fpm loads Xdebug" ""; fi
  # Xdebug must come after OPcache in the loading order (it does, OPcache is built in).
  x="$("$PHP" -n -d "zend_extension=$XDEBUG" -d opcache.enable_cli=1 -d xdebug.mode=develop -r 'echo extension_loaded("xdebug") ? "xdebug-on" : "xdebug-off", ",", opcache_get_status(false)["opcache_enabled"] ? "opcache-on" : "opcache-off";' 2>&1 | head -c 200)"
  case "$x" in
    xdebug-on,opcache-on) record PASS "Xdebug (develop) together with OPcache" "$x" ;;
    *) record FAIL "Xdebug (develop) together with OPcache" "$x" ;;
  esac
elif [ "$DLOPEN" = "true" ]; then
  record FAIL "Xdebug shared build" "no xdebug.so was produced"
else
  record INFO "Xdebug shared build" "skipped: static musl cannot load a .so (ADR 0010)"
fi

# 5. Literal php -r checks from the task -------------------------------------
r="$("$PHP" -n -r '$p = new PDO("sqlite::memory:"); $p->exec("create table t (a int)"); $p->exec("insert into t values (42)"); echo $p->query("select a from t")->fetchColumn();' 2>&1)"
if [ "$r" = "42" ]; then record PASS "php -r: SQLite in-memory PDO query" "$r"; else record FAIL "php -r: SQLite in-memory PDO query" "$r"; fi
r="$("$PHP" -n -r 'echo (new NumberFormatter("en_US", NumberFormatter::DECIMAL))->format(1234567.891);' 2>&1)"
if [ "$r" = "1,234,567.891" ]; then record PASS "php -r: intl NumberFormatter" "$r"; else record FAIL "php -r: intl NumberFormatter" "$r"; fi
r="$("$PHP" -n -d opcache.enable_cli=1 -r 'echo opcache_get_status(false)["opcache_enabled"] ? "on" : "off";' 2>&1)"
if [ "$r" = "on" ]; then record PASS "OPcache enabled with opcache.enable_cli=1" "$r"; else record FAIL "OPcache enabled with opcache.enable_cli=1" "$r"; fi
r="$("$PHP" -n -d opcache.enable_cli=1 -d opcache.jit=tracing -d opcache.jit_buffer_size=32M -r '$s = opcache_get_status(false); echo ($s["jit"]["enabled"] ?? false) ? "jit-on" : "jit-off", " ", ($s["jit"]["on"] ?? false) ? "jit-active" : "jit-inactive";' 2>&1)"
record INFO "OPcache JIT with jit=tracing" "$r"

# 6. Runtime checks (one PHP script, PASS/FAIL/INFO lines) ----------------------
CAFILE="${CAFILE:-}" "$PHP" -n "$HERE/runtime-checks.php" > "$OUT/runtime-checks.txt" 2>&1
rc_exit=$?
while IFS=$'\t' read -r status name detail; do
  case "$status" in
    PASS | FAIL | INFO) record "$status" "$name" "$detail" ;;
    *) record INFO "runtime-checks output" "$status $name $detail" ;;
  esac
done < "$OUT/runtime-checks.txt"
if [ "$rc_exit" -ne 0 ] && ! grep -q '^FAIL' "$OUT/runtime-checks.txt"; then
  record FAIL "runtime-checks.php exit code" "$rc_exit (crashed?)"
fi

# 7. One real request through php-fpm -------------------------------------------
FPM_DIR="$OUT/fpm"
mkdir -p "$FPM_DIR"
cat > "$FPM_DIR/fpm.conf" <<CONF
[global]
error_log = $FPM_DIR/fpm.log
daemonize = no
[www]
listen = 127.0.0.1:19000
pm = static
pm.max_children = 1
CONF
cat > "$FPM_DIR/hello.php" <<'PHPF'
<?php echo 'fpm-ok ', PHP_SAPI, ' ', in_array('sqlite', PDO::getAvailableDrivers(), true) ? 'sqlite' : 'nosqlite';
PHPF
"$FPM" -n -t -y "$FPM_DIR/fpm.conf" > "$FPM_DIR/test.txt" 2>&1
if grep -q 'test is successful' "$FPM_DIR/test.txt"; then record PASS "php-fpm -t (config test)" ""; else record FAIL "php-fpm -t (config test)" "$(head -c 200 "$FPM_DIR/test.txt")"; fi
"$FPM" -n -y "$FPM_DIR/fpm.conf" -F > "$FPM_DIR/run.txt" 2>&1 &
fpm_pid=$!
r="$("$PHP" -n "$HERE/fcgi-get.php" 127.0.0.1 19000 "$FPM_DIR/hello.php" 2>&1)"
kill "$fpm_pid" 2> /dev/null
wait "$fpm_pid" 2> /dev/null
if [ "$r" = "fpm-ok fpm-fcgi sqlite" ]; then record PASS "FastCGI request through php-fpm" "$r"; else record FAIL "FastCGI request through php-fpm" "$r $(tail -3 "$FPM_DIR/fpm.log" 2> /dev/null)"; fi

# 8. What the binary links against (information) --------------------------------
case "$(uname -s)" in
  Darwin)
    record INFO "otool -L" "$(otool -L "$PHP" | tail -n +2 | awk '{print $1}' | tr '\n' ' ')"
    record INFO "minimum macOS" "$(otool -l "$PHP" | awk '/LC_BUILD_VERSION/{f=1} f&&/minos/{print $2; exit}')"
    ;;
  *)
    record INFO "ldd" "$(ldd "$PHP" 2>&1 | head -8 | tr '\n' ' ')"
    record INFO "highest GLIBC symbol version needed" "$(objdump -T "$PHP" 2> /dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
    ;;
esac

echo "PASS=$pass FAIL=$fail" | tee "$OUT/smoke-status.txt"
[ "$fail" -eq 0 ]
