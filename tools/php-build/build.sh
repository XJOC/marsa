#!/usr/bin/env bash
# Builds PHP (cli + fpm, and the shared extensions) with spc: doctor, download, build.
# Run setup.sh first. Needs about 5 GB of disk: run it on a CI runner, not on a laptop.
#
#   WORK=/tmp/w PHP_VERSION=8.4.27 XDEBUG_VERSION=3.5.3 build.sh
#
# Environment:
#   WORK            the folder setup.sh filled; spc writes buildroot/, source/,
#                   downloads/, pkgroot/ and log/ here
#   PHP_VERSION     a full version such as 8.4.27 (never just 8.4)
#   XDEBUG_VERSION  the Xdebug release. spc's default source is the newest commit
#                   of xdebug/xdebug (3.6.0alpha1 in S-10 run 1), not a release.
#   SPC_TARGET      Linux: native-native-gnu.2.17 (glibc 2.17 with zig, can load a
#                   .so). A static musl build cannot load Xdebug (ADR 0012), so
#                   this script refuses to run on Linux without it.
#   EXTRA_LIBS      optional libraries for gd and redis
#                   (default: freetype,libjpeg,libwebp,zstd,liblz4)
#
# Every library is built from source: no --prefer-pre-built (ADR 0012).
# Writes $WORK/timings.txt.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

: "${WORK:?WORK is required}" "${PHP_VERSION:?PHP_VERSION is required}" "${XDEBUG_VERSION:?XDEBUG_VERSION is required}"
EXTRA_LIBS="${EXTRA_LIBS-freetype,libjpeg,libwebp,zstd,liblz4}"
SPC="$WORK/spc"

if ! [[ "$PHP_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "PHP_VERSION must be a full version such as 8.4.27, not $PHP_VERSION" >&2
  exit 1
fi
if [ "$(uname -s)" = Linux ] && [ -z "${SPC_TARGET:-}" ]; then
  echo "Linux needs SPC_TARGET=native-native-gnu.2.17: a musl build cannot load Xdebug" >&2
  exit 1
fi
if [ -n "${SPC_TARGET:-}" ]; then export SPC_TARGET; else unset SPC_TARGET || true; fi

EXTS="$(spc_names static)"
SHARED="$(spc_names shared)"
XDEBUG_URL="https://xdebug.org/files/xdebug-${XDEBUG_VERSION}.tgz"

cd "$WORK"
echo "spc:     $("$SPC" --version 2>&1 | head -1)"
echo "php:     $PHP_VERSION"
echo "static:  $EXTS"
echo "shared:  ${SHARED:-<none>}"
echo "libs:    ${EXTRA_LIBS:-<none>}"
echo "target:  ${SPC_TARGET:-<default>}"

now() { date +%s; }

t0="$(now)"
echo "::group::spc doctor"
# --auto-fix installs build tools (brew on macOS, apt on Ubuntu). It needs sudo
# on Linux; a CI runner has it. zig is already in place (setup.sh).
"$SPC" doctor --auto-fix
echo "::endgroup::"
t1="$(now)"

dl_args=(download "--with-php=$PHP_VERSION" "--for-extensions=${EXTS}${SHARED:+,$SHARED}" --ignore-cache-sources=php-src --retry=2)
if [ -n "$EXTRA_LIBS" ]; then dl_args+=("--for-libs=$EXTRA_LIBS"); fi
case ",$SHARED," in *,xdebug,*) dl_args+=("--custom-url=xdebug:$XDEBUG_URL") ;; esac
echo "::group::spc ${dl_args[*]}"
"$SPC" "${dl_args[@]}"
echo "::endgroup::"
t2="$(now)"

# spc does not check a custom URL, so check the pinned hash here.
case ",$SHARED," in *,xdebug,*) check_pinned "downloads/$(basename "$XDEBUG_URL")" ;; esac

build_args=(build "$EXTS" --build-cli --build-fpm)
if [ -n "$SHARED" ]; then build_args+=("--build-shared=$SHARED"); fi
if [ -n "$EXTRA_LIBS" ]; then build_args+=("--with-libs=$EXTRA_LIBS"); fi
echo "::group::spc ${build_args[*]}"
"$SPC" "${build_args[@]}"
echo "::endgroup::"
t3="$(now)"

{
  echo "T_DOCTOR=$((t1 - t0))"
  echo "T_DOWNLOAD=$((t2 - t1))"
  echo "T_BUILD=$((t3 - t2))"
  echo "T_TOTAL=$((t3 - t0))"
} > "$WORK/timings.txt"
cat "$WORK/timings.txt"
ls -l buildroot/bin buildroot/modules
