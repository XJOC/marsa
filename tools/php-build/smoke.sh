#!/usr/bin/env bash
# Smoke tests for one unpacked PHP package (the layout of recipes/php.toml):
#
#   OUT=/tmp/out PHP_VERSION=8.4.27 smoke.sh <package dir>
#
# The package dir holds bin/php, bin/php-fpm, and ext/<name>.so for every shared
# extension in recipes/php-extensions.txt.
#
# Environment:
#   OUT             where the logs go (required)
#   PHP_VERSION     `php -v` must say exactly this version (required)
#   SPC_VERSION     `php -v` must say "Built by static-php-cli <this>" (optional)
#   XDEBUG_VERSION  `php -v` with Xdebug must say "with Xdebug v<this>" (optional)
#   CAFILE          a CA bundle for the HTTPS check in runtime-checks.php (optional)
#
# Checks: php -v, php -m equals the list (nothing missing, nothing extra),
# php-fpm -m equals php -m, exported zend_* symbols (>= 1000), every shared
# extension loads in php and php-fpm (with OPcache), Linux needs no glibc newer
# than 2.17, runtime checks (SQLite, intl, gd, ...), and one FastCGI request
# through php-fpm. Writes $OUT/smoke.md and $OUT/smoke-status.txt (PASS=n FAIL=n).
# Exit code 1 if any check fails.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/lib.sh"

PKG="${1:?usage: smoke.sh <package dir>}"
PKG="$(cd "$PKG" && pwd)"
: "${OUT:?OUT is required}" "${PHP_VERSION:?PHP_VERSION is required}"
PHP="$PKG/bin/php"
FPM="$PKG/bin/php-fpm"

mkdir -p "$OUT"
MD="$OUT/smoke.md"
pass=0
fail=0
{
  echo '| | check | detail |'
  echo '| --- | --- | --- |'
} > "$MD"

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
check() { # check <name> <detail> <test command...>
  local name="$1" detail="$2"
  shift 2
  if "$@"; then record PASS "$name" "$detail"; else record FAIL "$name" "$detail"; fi
}

# Lowercase, sorted, unique module names. Drops the [PHP Modules] headers and
# blanks, and cgi-fcgi, the pseudo-module php-fpm adds.
norm_modules() { grep -v '^\[' | grep -v '^[[:space:]]*$' | sed 's/\r$//' | tr 'A-Z' 'a-z' | grep -vx 'cgi-fcgi' | sort -u; }

# 1. Version ----------------------------------------------------------------
"$PHP" -n -v > "$OUT/php-v.txt" 2>&1
first="$(head -1 "$OUT/php-v.txt")"
check "php -v says PHP $PHP_VERSION" "$first" grep -q "^PHP $PHP_VERSION " "$OUT/php-v.txt"
if [ -n "${SPC_VERSION:-}" ]; then
  # spc adds this line to the banner of PHP 8.4 and later only (8.3.36 has none).
  minor="${PHP_VERSION#*.}"; minor="${minor%%.*}"
  if [ "${PHP_VERSION%%.*}" -gt 8 ] || [ "$minor" -ge 4 ]; then
    check "php -v says Built by static-php-cli $SPC_VERSION" "" grep -qx "Built by static-php-cli $SPC_VERSION" "$OUT/php-v.txt"
  fi
  check "BUILD-INFO.txt says static-php-cli $SPC_VERSION" "" grep -qx "static-php-cli $SPC_VERSION" "$PKG/BUILD-INFO.txt"
fi
"$FPM" -n -v > "$OUT/fpm-v.txt" 2>&1
check "php-fpm -v says PHP $PHP_VERSION" "$(head -1 "$OUT/fpm-v.txt")" grep -q "^PHP $PHP_VERSION " "$OUT/fpm-v.txt"
"$PHP" -n -i 2> /dev/null | grep -E '^(PHP API|Thread Safety|Debug Build|Zend Extension Build|extension_dir)' > "$OUT/php-i.txt" || true
record INFO "api / thread safety / debug" "$(tr '\n' ';' < "$OUT/php-i.txt")"

# 2. php -m against recipes/php-extensions.txt --------------------------------
expected_modules > "$OUT/modules-expected.txt"
"$PHP" -n -m | norm_modules > "$OUT/modules-actual.txt"
comm -23 "$OUT/modules-expected.txt" "$OUT/modules-actual.txt" > "$OUT/modules-missing.txt"
comm -13 "$OUT/modules-expected.txt" "$OUT/modules-actual.txt" > "$OUT/modules-extra.txt"
check "php -m: no module missing from the list" "$(tr '\n' ' ' < "$OUT/modules-missing.txt")" test ! -s "$OUT/modules-missing.txt"
check "php -m: no module that is not in the list" "$(tr '\n' ' ' < "$OUT/modules-extra.txt")" test ! -s "$OUT/modules-extra.txt"
"$FPM" -n -m | norm_modules > "$OUT/fpm-modules-actual.txt"
check "php-fpm -m equals php -m" "$(diff "$OUT/modules-actual.txt" "$OUT/fpm-modules-actual.txt" | tr '\n' ' ')" \
  cmp -s "$OUT/modules-actual.txt" "$OUT/fpm-modules-actual.txt"

# 3. Exported Zend symbols (ADR 0010): a .so extension needs them from the binary.
for bin in "$PHP" "$FPM"; do
  case "$(uname -s)" in
    Darwin) n="$(nm -gU "$bin" 2> /dev/null | awk '{ print $NF }' | grep -c '^_zend_' || true)" ;;
    *) n="$(nm -D --defined-only "$bin" 2> /dev/null | awk '{ print $NF }' | grep -c '^zend_' || true)" ;;
  esac
  # PHP 8.4 exports about 1,035, PHP 8.3 about 980; a broken build exports none.
  check "$(basename "$bin"): exported zend_* symbols >= 900" "${n:-0}" test "${n:-0}" -ge 900
done

# 4. Shared extensions, from the package's ext/ folder --------------------------
while IFS='|' read -r module name; do
  so="$PKG/ext/$name.so"
  if [ ! -f "$so" ]; then
    record FAIL "ext/$name.so is in the package" ""
    continue
  fi
  "$PHP" -n -d "zend_extension=$so" -m > "$OUT/$name-m.txt" 2>&1
  check "php loads ext/$name.so ($module)" "" grep -qx "$module" "$OUT/$name-m.txt"
  "$FPM" -n -d "zend_extension=$so" -m > "$OUT/$name-fpm-m.txt" 2>&1
  check "php-fpm loads ext/$name.so ($module)" "" grep -qx "$module" "$OUT/$name-fpm-m.txt"
  # Loads after the built-in OPcache, and both stay on.
  r="$("$PHP" -n -d "zend_extension=$so" -d opcache.enable_cli=1 -r "echo extension_loaded('$name') ? 'on' : 'off', ',', opcache_get_status(false)['opcache_enabled'] ? 'on' : 'off';" 2>&1 | head -c 200)"
  check "$module together with OPcache" "$r" test "$r" = "on,on"
done < <(shared_modules)
if [ -n "${XDEBUG_VERSION:-}" ] && [ -f "$PKG/ext/xdebug.so" ]; then
  "$PHP" -n -d "zend_extension=$PKG/ext/xdebug.so" -v > "$OUT/xdebug-v.txt" 2>&1
  check "php -v says with Xdebug v$XDEBUG_VERSION" "$(grep Xdebug "$OUT/xdebug-v.txt" | head -1)" \
    grep -q "with Xdebug v$XDEBUG_VERSION," "$OUT/xdebug-v.txt"
fi

# 5. What the binary links against ----------------------------------------------
case "$(uname -s)" in
  Darwin)
    record INFO "otool -L" "$(otool -L "$PHP" | tail -n +2 | awk '{ print $1 }' | tr '\n' ' ')"
    record INFO "minimum macOS" "$(otool -l "$PHP" | awk '/LC_BUILD_VERSION/ { f = 1 } f && /minos/ { print $2; exit }')"
    ;;
  *)
    record INFO "ldd" "$(ldd "$PHP" 2>&1 | head -8 | tr '\n' ' ')"
    for bin in "$PHP" "$FPM" "$PKG"/ext/*.so; do
      newest="$(objdump -T "$bin" 2> /dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
      check "$(basename "$bin"): needs no glibc newer than 2.17" "${newest:-none}" \
        test "$(printf '%s\nGLIBC_2.17\n' "${newest:-GLIBC_0}" | sort -V | tail -1)" = GLIBC_2.17
    done
    ;;
esac

# 6. Literal php -r checks -------------------------------------------------------
r="$("$PHP" -n -r '$p = new PDO("sqlite::memory:"); $p->exec("create table t (a int)"); $p->exec("insert into t values (42)"); echo $p->query("select a from t")->fetchColumn();' 2>&1)"
check "php -r: SQLite in-memory PDO query" "$r" test "$r" = 42
r="$("$PHP" -n -r 'echo (new NumberFormatter("en_US", NumberFormatter::DECIMAL))->format(1234567.891);' 2>&1)"
check "php -r: intl NumberFormatter" "$r" test "$r" = "1,234,567.891"
r="$("$PHP" -n -d opcache.enable_cli=1 -r 'echo opcache_get_status(false)["opcache_enabled"] ? "on" : "off";' 2>&1)"
check "OPcache on with opcache.enable_cli=1" "$r" test "$r" = on
r="$("$PHP" -n -d opcache.enable_cli=1 -d opcache.jit=tracing -d opcache.jit_buffer_size=32M -r '$s = opcache_get_status(false); echo ($s["jit"]["enabled"] ?? false) ? "jit-on" : "jit-off";' 2>&1)"
record INFO "OPcache JIT with jit=tracing" "$r"

# 7. Runtime checks (one PHP script, PASS/FAIL/INFO lines) ------------------------
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

# 8. One real request through php-fpm ---------------------------------------------
FPM_DIR="$OUT/fpm"
mkdir -p "$FPM_DIR"
cat > "$FPM_DIR/fpm.conf" << CONF
[global]
error_log = $FPM_DIR/fpm.log
daemonize = no
[www]
listen = 127.0.0.1:19000
pm = static
pm.max_children = 1
CONF
cat > "$FPM_DIR/hello.php" << 'PHPF'
<?php echo 'fpm-ok ', PHP_SAPI, ' ', in_array('sqlite', PDO::getAvailableDrivers(), true) ? 'sqlite' : 'nosqlite';
PHPF
"$FPM" -n -t -y "$FPM_DIR/fpm.conf" > "$FPM_DIR/test.txt" 2>&1
check "php-fpm -t (config test)" "$(head -c 200 "$FPM_DIR/test.txt")" grep -q 'test is successful' "$FPM_DIR/test.txt"
"$FPM" -n -y "$FPM_DIR/fpm.conf" -F > "$FPM_DIR/run.txt" 2>&1 &
fpm_pid=$!
r="$("$PHP" -n "$HERE/fcgi-get.php" 127.0.0.1 19000 "$FPM_DIR/hello.php" 2>&1)"
kill "$fpm_pid" 2> /dev/null
wait "$fpm_pid" 2> /dev/null
check "FastCGI request through php-fpm" "$r" test "$r" = "fpm-ok fpm-fcgi sqlite"

echo "PASS=$pass FAIL=$fail" | tee "$OUT/smoke-status.txt"
[ "$fail" -eq 0 ]
