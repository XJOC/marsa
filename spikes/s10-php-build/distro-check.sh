#!/usr/bin/env bash
# S-10: does the PHP we built run on other Linux distributions? (Linux runner with docker.)
#
#   distro-check.sh <buildroot>
#
# For each image: php -v, a SQLite query, an intl call, and (when a shared Xdebug was
# built) loading xdebug.so. A glibc 2.17 build should run on all glibc images and fail
# on Alpine (musl); a static musl build should run everywhere but cannot load xdebug.so.
# Writes $OUT/distros.md. Always exits 0: the result is data, not a gate.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${OUT:-$HERE/out}"
BR="${1:?usage: distro-check.sh <buildroot>}"
BR="$(cd "$BR" && pwd)"
mkdir -p "$OUT"

images="centos:7 ubuntu:18.04 debian:12 fedora:latest alpine:3.21"
{
  echo '| image | libc | php -v | sqlite + intl | xdebug.so |'
  echo '| --- | --- | --- | --- | --- |'
} > "$OUT/distros.md"

run() { docker run --rm --platform "$PLATFORM" -v "$BR:/b:ro" "$@" 2>&1; }
case "$(uname -m)" in aarch64 | arm64) PLATFORM=linux/arm64 ;; *) PLATFORM=linux/amd64 ;; esac

for img in $images; do
  if ! docker pull -q --platform "$PLATFORM" "$img" > /dev/null 2>&1; then
    echo "| $img | - | image not available | - | - |" >> "$OUT/distros.md"
    continue
  fi
  libc="$(run "$img" sh -c 'if ls /lib/ld-musl-* > /dev/null 2>&1; then echo musl; else ldd --version 2>&1 | head -1 | grep -o "[0-9]*\.[0-9]*$" | sed "s/^/glibc /"; fi' | head -1)"
  v="$(run "$img" /b/bin/php -n -v | head -1 | cut -c1-60)"
  case "$v" in PHP\ 8.*) v="ok: $v" ;; *) v="FAIL: $(printf '%s' "$v" | cut -c1-70)" ;; esac
  q="$(run "$img" /b/bin/php -n -r '$p = new PDO("sqlite::memory:"); echo $p->query("select 42")->fetchColumn(), " ", (new NumberFormatter("en_US", NumberFormatter::DECIMAL))->format(1234.5);' | head -1)"
  case "$q" in "42 1,234.5") q="ok" ;; *) q="FAIL: $(printf '%s' "$q" | cut -c1-70)" ;; esac
  if [ -f "$BR/modules/xdebug.so" ]; then
    x="$(run "$img" /b/bin/php -n -d zend_extension=/b/modules/xdebug.so -m | grep -x Xdebug | head -1)"
    if [ "$x" = "Xdebug" ]; then x="ok"; else x="FAIL"; fi
  else
    x="n/a (no .so built)"
  fi
  echo "| $img | $libc | $v | $q | $x |" >> "$OUT/distros.md"
done
cat "$OUT/distros.md"
exit 0
