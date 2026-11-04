#!/usr/bin/env bash
# S-10: download sources and build static PHP (cli + fpm + shared Xdebug) with spc.
#
# Run on a CI runner (or any machine with a lot of disk: ~5 GB). Do not run it on
# a full laptop disk. Everything goes to work/ (git-ignored).
#
#   SPC=work/spc PHP_VERSION=8.4 ./build.sh
#
# Environment (all optional):
#   SPC               path to the spc binary           (default: work/spc)
#   PHP_VERSION       "8.4" (newest 8.4.x) or "8.4.23"  (default: 8.4)
#   SPC_TARGET        Linux only. "native-native-gnu.2.17" = glibc 2.17 build that
#                     can dlopen a .so. Empty = static musl (cannot load .so).
#   SHARED            shared extensions (default: all names in shared-extensions.txt;
#                     set to "" to skip, needed for static musl)
#   PREFER_PRE_BUILT  "true" = use static-php.dev pre-built libraries when they
#                     match; "false" = build every library from source (default: true)
#   EXTRA_LIBS        optional gd/redis libraries (default: freetype,libjpeg,libwebp,zstd,liblz4)
#   XDEBUG_URL        a source archive for Xdebug that replaces spc's default. The
#                     default is the newest commit of the xdebug/xdebug repo (3.6.0alpha1
#                     in run 1), not a release. Needs XDEBUG_SHA256 too.
#   XDEBUG_SHA256     sha256 of that archive; the build stops if it differs
#   OUT               where timings.txt is written     (default: out/)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${WORK:-$HERE/work}"
OUT="${OUT:-$HERE/out}"
SPC="${SPC:-$WORK/spc}"
PHP_VERSION="${PHP_VERSION:-8.4}"
PREFER_PRE_BUILT="${PREFER_PRE_BUILT:-true}"
EXTRA_LIBS="${EXTRA_LIBS-freetype,libjpeg,libwebp,zstd,liblz4}"

# Join the non-comment, non-empty lines of a file with commas.
join_list() { grep -v '^[[:space:]]*#' "$1" | grep -v '^[[:space:]]*$' | paste -sd, -; }

EXTS="$(join_list "$HERE/extensions.txt")"
SHARED="${SHARED-$(join_list "$HERE/shared-extensions.txt")}"

if [ -n "${SPC_TARGET:-}" ]; then export SPC_TARGET; else unset SPC_TARGET || true; fi

mkdir -p "$WORK" "$OUT"
cd "$WORK"   # spc writes buildroot/, source/, downloads/, pkgroot/ and log/ in the current folder.

echo "spc:        $("$SPC" --version 2>&1 | head -1)"
echo "php:        $PHP_VERSION"
echo "static:     $EXTS"
echo "shared:     ${SHARED:-<none>}"
echo "libs:       ${EXTRA_LIBS:-<none>}"
echo "target:     ${SPC_TARGET:-<default>}"
echo "pre-built:  $PREFER_PRE_BUILT"

now() { date +%s; }

t0="$(now)"
echo "::group::spc doctor"
# doctor --auto-fix installs the build tools (brew on macOS, apt on Ubuntu, the zig
# toolchain for glibc). It needs sudo on Linux; a CI runner has it.
"$SPC" doctor --auto-fix
echo "::endgroup::"
t1="$(now)"

dl_args=(download "--with-php=$PHP_VERSION" "--for-extensions=${EXTS}${SHARED:+,$SHARED}" --ignore-cache-sources=php-src --retry=2)
if [ -n "$EXTRA_LIBS" ]; then dl_args+=("--for-libs=$EXTRA_LIBS"); fi
if [ "$PREFER_PRE_BUILT" = "true" ]; then dl_args+=(--prefer-pre-built); fi
XDEBUG_URL="${XDEBUG_URL:-}"
case ",$SHARED," in *,xdebug,*) ;; *) XDEBUG_URL="" ;; esac   # only when Xdebug is built
if [ -n "$XDEBUG_URL" ]; then dl_args+=("--custom-url=xdebug:$XDEBUG_URL"); fi
echo "::group::spc ${dl_args[*]}"
"$SPC" "${dl_args[@]}"
echo "::endgroup::"
t2="$(now)"

# spc does not check a custom URL, so check the pinned hash here.
if [ -n "$XDEBUG_URL" ]; then
  want="${XDEBUG_SHA256:?XDEBUG_SHA256 is required with XDEBUG_URL}"
  file="downloads/$(basename "$XDEBUG_URL")"
  if [ ! -f "$file" ]; then
    echo "Xdebug archive $file not found; downloads/ has:"
    ls -l downloads
    exit 1
  fi
  got="$( (sha256sum "$file" 2> /dev/null || shasum -a 256 "$file") | cut -d' ' -f1)"
  if [ "$got" != "$want" ]; then
    echo "Xdebug sha256 mismatch: wanted $want, got $got"
    exit 1
  fi
  echo "Xdebug archive ok: $file sha256 $got"
fi

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
} > "$OUT/timings.txt"
cat "$OUT/timings.txt"

ls -l buildroot/bin buildroot/modules 2>/dev/null || true
