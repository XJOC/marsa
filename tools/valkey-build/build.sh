#!/usr/bin/env bash
# Builds Valkey from the pinned source for the machine it runs on: macOS, or
# Linux inside an Alpine container (static musl). Windows has its own script,
# build-cygwin.sh.
#
#   VALKEY_VERSION=8.1.11 PLATFORM=darwin-arm64 build.sh <output dir>
#
# PLATFORM is a Marsa platform key: darwin-arm64, darwin-x64, linux-x64,
# linux-arm64. The output dir gets:
#
#   pkg/                                the unpacked package (smoke.sh checks it)
#   dist/valkey-<version>-<platform>.tar.gz and its .sha256
#
# The package:
#
#   bin/valkey-server, bin/valkey-cli
#   licenses/                           Valkey's COPYING, the notices of the code
#                                       it bundles, NOTICE.txt, and musl's COPYRIGHT on Linux
#   BUILD-INFO.txt
#
# macOS needs Xcode's command line tools, curl and tar. Linux (Alpine 3.22,
# as root) needs: bash build-base linux-headers curl tar file coreutils binutils.
set -euo pipefail
export LC_ALL=C

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

: "${VALKEY_VERSION:?VALKEY_VERSION is required}" "${PLATFORM:?PLATFORM is required}"
OUT_ARG="${1:?usage: build.sh <output dir>}"
[[ "$VALKEY_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "VALKEY_VERSION must look like 8.1.11, got: $VALKEY_VERSION" >&2; exit 1; }
case "$PLATFORM" in
  darwin-arm64 | darwin-x64 | linux-x64 | linux-arm64) ;;
  *) echo "unknown PLATFORM $PLATFORM" >&2; exit 1 ;;
esac

if [ -n "${EXPECT_ARCH:-}" ] && [ "$(uname -m)" != "$EXPECT_ARCH" ]; then
  echo "this machine is $(uname -m), expected $EXPECT_ARCH" >&2
  exit 1
fi

# musl on Alpine 3.22. The pinned source tarball gives us musl's COPYRIGHT; it
# must be the version the static binary links.
MUSL_VERSION="1.2.5"

mkdir -p "$OUT_ARG"
OUT="$(cd "$OUT_ARG" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PKG="$OUT/pkg"
NAME="valkey-$VALKEY_VERSION-$PLATFORM.tar.gz"

fetch_valkey_source "$WORK"

case "$PLATFORM" in
  darwin-*)
    # The binary must run on macOS 12 (Marsa's floor). The default malloc on
    # macOS is already libc; it is explicit so a Makefile change shows up.
    export MACOSX_DEPLOYMENT_TARGET=12.0
    (cd "$VALKEY_SRC" && make -j"$(ncpu)" BUILD_TLS=no MALLOC=libc valkey-server valkey-cli)
    ;;
  linux-*)
    command -v apk > /dev/null || { echo "the Linux build runs in an Alpine container (apk not found)" >&2; exit 1; }
    musl_installed="$(apk info -v | grep -E '^musl-[0-9]' | head -n 1)"
    case "$musl_installed" in
      "musl-$MUSL_VERSION-r"*) ;;
      *) echo "this Alpine has $musl_installed, but the pinned musl source is $MUSL_VERSION" >&2; exit 1 ;;
    esac
    # CFLAGS and LDFLAGS both get -static so the dependencies (Lua, hiredis)
    # are compiled and linked the same way. libc malloc keeps the idle server
    # small (3.5 MB in the S-12 measurement) and needs no jemalloc build.
    (cd "$VALKEY_SRC" && make -j"$(ncpu)" BUILD_TLS=no MALLOC=libc CFLAGS=-static LDFLAGS=-static valkey-server valkey-cli)
    ;;
esac

rm -rf "$PKG"
mkdir -p "$PKG/bin" "$PKG/licenses" "$OUT/dist"
cp "$VALKEY_SRC/src/valkey-server" "$VALKEY_SRC/src/valkey-cli" "$PKG/bin/"
case "$PLATFORM" in
  darwin-*)
    strip -x "$PKG/bin/valkey-server" "$PKG/bin/valkey-cli"
    # strip invalidates the linker's signature. arm64 macOS kills an unsigned
    # binary, so sign it again (ad hoc, no certificate).
    codesign --force --sign - "$PKG/bin/valkey-server" "$PKG/bin/valkey-cli"
    ;;
  linux-*) strip --strip-unneeded "$PKG/bin/valkey-server" "$PKG/bin/valkey-cli" ;;
esac

collect_valkey_licenses "$VALKEY_SRC" "$PKG/licenses"
{
  write_notice_head
  case "$PLATFORM" in
    linux-*)
      echo
      echo "The binaries are linked statically against musl libc $MUSL_VERSION (MIT licence), whose"
      echo "licence and the notices of the code it bundles are in musl.COPYRIGHT."
      ;;
  esac
} > "$PKG/licenses/NOTICE.txt"
case "$PLATFORM" in
  linux-*)
    fetch_pinned "https://musl.libc.org/releases/musl-$MUSL_VERSION.tar.gz" "$WORK/musl-$MUSL_VERSION.tar.gz"
    tar -xzf "$WORK/musl-$MUSL_VERSION.tar.gz" -C "$WORK" "musl-$MUSL_VERSION/COPYRIGHT"
    cp "$WORK/musl-$MUSL_VERSION/COPYRIGHT" "$PKG/licenses/musl.COPYRIGHT"
    grep -q 'musl as a whole is licensed under the following standard MIT license' "$PKG/licenses/musl.COPYRIGHT" ||
      { echo "musl's COPYRIGHT is not the text we expect" >&2; exit 1; }
    ;;
esac

{
  echo "valkey $VALKEY_VERSION for $PLATFORM"
  echo "built $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "commit ${GITHUB_SHA:-unknown}"
  echo "workflow run ${GITHUB_SERVER_URL:-}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}"
  echo "runner image ${ImageOS:-container} ${ImageVersion:-}"
  echo "system $(uname -s) $(uname -r) $(uname -m)"
  echo "source: valkey-$VALKEY_VERSION.tar.gz sha256 $(sha256_of "$VALKEY_SRC_TARBALL") (unmodified)"
  case "$PLATFORM" in
    darwin-*)
      echo "make BUILD_TLS=no MALLOC=libc valkey-server valkey-cli, MACOSX_DEPLOYMENT_TARGET=12.0"
      echo "compiler: $(cc --version | head -n 1)"
      if command -v xcodebuild > /dev/null; then echo "xcode: $(xcodebuild -version | tr '\n' ' ')"; fi
      ;;
    linux-*)
      echo "make BUILD_TLS=no MALLOC=libc CFLAGS=-static LDFLAGS=-static valkey-server valkey-cli"
      echo "alpine: $(cat /etc/alpine-release)"
      echo "musl: $(apk info -v | grep -E '^musl-[0-9]' | head -n 1) (licence text from musl-$MUSL_VERSION.tar.gz)"
      echo "compiler: $(gcc --version | head -n 1)"
      echo "linker: $(ld --version | head -n 1)"
      ;;
  esac
  echo "valkey-server sha256 $(sha256_of "$PKG/bin/valkey-server")"
  echo "valkey-cli sha256 $(sha256_of "$PKG/bin/valkey-cli")"
  echo "license: BSD-3-Clause (Valkey), see licenses/"
} > "$PKG/BUILD-INFO.txt"

chmod 755 "$PKG/bin/valkey-server" "$PKG/bin/valkey-cli"
chmod 644 "$PKG/BUILD-INFO.txt"
find "$PKG/licenses" -type f -exec chmod 644 {} +
# No macOS extended attributes or ._ files in the archive.
if command -v xattr > /dev/null; then xattr -cr "$PKG"; fi
COPYFILE_DISABLE=1 tar -czf "$OUT/dist/$NAME" -C "$PKG" BUILD-INFO.txt bin licenses
(cd "$OUT/dist" && printf '%s  %s\n' "$(sha256_of "$NAME")" "$NAME" > "$NAME.sha256")
tar -tzvf "$OUT/dist/$NAME"
cat "$OUT/dist/$NAME.sha256"
# The caller (outside the container) must be able to read and rename these.
chmod -R a+rX "$OUT"
