#!/usr/bin/env bash
# S-03 path C: build ONLY xdebug.so from Xdebug source + the matching PHP 8.4 headers,
# to load it into the prebuilt static-php-cli PHP.
#   1. configure PHP source with --disable-all (default NTS, no debug, same ABI as the
#      prebuilt binary) and install only phpize/php-config/headers/build files. No PHP compile.
#   2. phpize + configure + make Xdebug against that tree.
# Needs 00-tools.sh and 10-fetch.sh first. No sudo, nothing outside work/.
# Run:  ./30-build-xdebug-so.sh      (about 3 to 5 minutes)
# Output: work/out/xdebug-<php-minor>-macos-arm64.so and results/build-xdebug-so.txt
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

need_disk_gb 8
JOBS="$(sysctl -n hw.ncpu)"
PHPSRC="$WORK/src/php-$PHP_VERSION"
PHPDEV="$WORK/phpdev"
XSRC="$WORK/src/xdebug-$XDEBUG_VERSION"
OUT="$WORK/out"
PHP_MINOR="${PHP_VERSION%.*}"
mkdir -p "$OUT"
START=$(date +%s)

# Keep the build deterministic and local.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

if [ ! -x "$PHPDEV/bin/phpize" ]; then
  ( cd "$PHPSRC" \
    && ./configure --prefix="$PHPDEV" --disable-all --disable-cgi --disable-phpdbg > "$WORK/php-configure.log" 2>&1 \
    && make install-headers install-build install-programs > "$WORK/php-install-headers.log" 2>&1 )
fi
"$PHPDEV/bin/php-config" --version >/dev/null

if [ ! -f "$XSRC/modules/xdebug.so" ]; then
  ( cd "$XSRC" \
    && "$PHPDEV/bin/phpize" > "$WORK/xdebug-phpize.log" 2>&1 \
    && ./configure --enable-xdebug --with-php-config="$PHPDEV/bin/php-config" > "$WORK/xdebug-configure.log" 2>&1 \
    && make -j"$JOBS" > "$WORK/xdebug-make.log" 2>&1 )
fi

SO="$OUT/xdebug-$PHP_MINOR-macos-arm64.so"
cp "$XSRC/modules/xdebug.so" "$SO"
END=$(date +%s)
{
  echo "xdebug $XDEBUG_VERSION built against php $PHP_VERSION headers"
  echo "build seconds (this run): $((END - START))"
  echo "== file / size / sha256"
  file "$SO"; stat -f '%z bytes' "$SO"; shasum -a 256 "$SO"
  echo "== codesign"; codesign -dv "$SO" 2>&1 | grep -E 'Format|Signature|flags' || true
  echo "== otool -L"; otool -L "$SO"
  echo "== undefined symbols resolved at load time (count; they must come from the php binary)"
  echo "undefined total: $(nm -u "$SO" | wc -l), of which Zend/PHP API: $(nm -u "$SO" | grep -c -E '^_(zend|php|_zend|_emalloc|_efree|_erealloc|OnUpdate|display_ini|display_)' || true)"
  nm -u "$SO" | grep -E '^_(zend_register_ini_entries|zend_execute_ex|zend_error_cb)$' || true
  echo "== php-config"; for o in --version --vernum --extension-dir; do echo "$o: $("$PHPDEV/bin/php-config" "$o")"; done
} > "$RESULTS/build-xdebug-so.txt" 2>&1
cat "$RESULTS/build-xdebug-so.txt"
