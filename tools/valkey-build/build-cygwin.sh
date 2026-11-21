#!/usr/bin/env bash
# Builds Valkey for Windows with Cygwin. Run it in a Cygwin bash (the workflow
# installs Cygwin with gcc-core make curl tar binutils bzip2 xz zip).
#
#   VALKEY_VERSION=8.1.11 build-cygwin.sh <output dir>
#
# Valkey has no Windows port. The plain POSIX build works under Cygwin with one
# change to a Cygwin header (not to Valkey's source), and only valkey-server and
# valkey-cli are built (valkey-benchmark is not needed). The output dir gets:
#
#   pkg/                                    the unpacked package (smoke.sh checks it)
#   dist/valkey-<version>-windows-x64.zip   and its .sha256
#   dist/cygwin-<cygwin version>-src.tar.xz the source of the cygwin1.dll we ship
#
# The package:
#
#   bin/valkey-server.exe, bin/valkey-cli.exe
#   bin/cygwin1.dll, bin/cyggcc_s-seh-1.dll
#   licenses/                               Valkey's licences, and the Cygwin and GCC runtime texts
#   BUILD-INFO.txt
#
# cygwin1.dll is LGPL-3.0 or later, so its exact source goes in the same
# release. build-cygwin.sh pins that source by sha256 (downloads.sha256) and
# stops if the installed Cygwin is not the pinned one.
set -euo pipefail
export LC_ALL=C

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

: "${VALKEY_VERSION:?VALKEY_VERSION is required}"
OUT_ARG="${1:?usage: build-cygwin.sh <output dir>}"
[[ "$VALKEY_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "VALKEY_VERSION must look like 8.1.11, got: $VALKEY_VERSION" >&2; exit 1; }
PLATFORM="windows-x64"

mkdir -p "$OUT_ARG"
OUT="$(cd "$OUT_ARG" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PKG="$OUT/pkg"
NAME="valkey-$VALKEY_VERSION-$PLATFORM.zip"

# The version of the installed Cygwin DLL package, for example 3.6.12-1.
package_version() { cygcheck -c "$1" | awk -v p="$1" '$1 == p { print $2 }'; }
CYGWIN_VERSION="$(package_version cygwin)"
LIBGCC_VERSION="$(package_version libgcc1)"
[ -n "$CYGWIN_VERSION" ] && [ -n "$LIBGCC_VERSION" ] || { echo "cannot read the Cygwin package versions" >&2; exit 1; }
CYGWIN_SRC="cygwin-$CYGWIN_VERSION-src.tar.xz"

# Stops here, before the long build, if there is no pinned source for this Cygwin.
mkdir -p "$OUT/dist"
fetch_pinned "https://mirrors.kernel.org/sourceware/cygwin/src/release/cygwin/$CYGWIN_SRC" "$OUT/dist/$CYGWIN_SRC"
fetch_pinned "https://raw.githubusercontent.com/gcc-mirror/gcc/releases/gcc-14.2.0/COPYING.RUNTIME" "$WORK/gcc-COPYING.RUNTIME-3.1"

fetch_valkey_source "$WORK"

# Valkey uses dladdr and Dl_info (crash reports), which Cygwin's dlfcn.h hides
# behind __GNU_VISIBLE. This is the one change redis-windows makes as well. It
# edits the Cygwin on this throw-away runner, not Valkey's source.
if grep -q __GNU_VISIBLE /usr/include/dlfcn.h; then
  sed -i 's/__GNU_VISIBLE/1/' /usr/include/dlfcn.h
fi

# -O2: the default -O3 with LTO is much slower to build here.
(
  cd "$VALKEY_SRC"
  make -j"$(ncpu)" BUILD_TLS=no OPTIMIZATION=-O2 CFLAGS="-Wno-char-subscripts" valkey-server valkey-cli
)

rm -rf "$PKG"
mkdir -p "$PKG/bin" "$PKG/licenses"
cp "$VALKEY_SRC/src/valkey-server.exe" "$VALKEY_SRC/src/valkey-cli.exe" "$PKG/bin/"
strip "$PKG/bin/valkey-server.exe" "$PKG/bin/valkey-cli.exe"

# The Cygwin DLLs the programs need, next to them. Anything else the programs
# load must come from Windows itself (ldd shows those under /cygdrive/c/Windows).
echo "--- ldd"
ldd "$PKG/bin/valkey-server.exe" "$PKG/bin/valkey-cli.exe"
dlls="$(ldd "$PKG/bin/valkey-server.exe" "$PKG/bin/valkey-cli.exe" | awk '$3 ~ /^\/usr\/bin\// { print $3 }' | sort -u)"
[ "$dlls" = "$(printf '/usr/bin/cyggcc_s-seh-1.dll\n/usr/bin/cygwin1.dll')" ] ||
  { echo "unexpected Cygwin DLLs: $dlls" >&2; exit 1; }
cp /usr/bin/cygwin1.dll /usr/bin/cyggcc_s-seh-1.dll "$PKG/bin/"

# Licence texts. Valkey's, then Cygwin's from its own source (the exact version
# we ship), then the GCC runtime exception.
collect_valkey_licenses "$VALKEY_SRC" "$PKG/licenses"
mkdir "$WORK/cygsrc"
tar -xJf "$OUT/dist/$CYGWIN_SRC" -C "$WORK/cygsrc"
inner="$(find "$WORK/cygsrc" -name 'newlib-cygwin-*.tar.bz2')"
[ -n "$inner" ] || { echo "no newlib-cygwin tarball in $CYGWIN_SRC" >&2; exit 1; }
tar -xjf "$inner" -C "$WORK/cygsrc" newlib-cygwin/winsup/CYGWIN_LICENSE newlib-cygwin/COPYING3.LIB newlib-cygwin/COPYING3
cp "$WORK/cygsrc/newlib-cygwin/winsup/CYGWIN_LICENSE" "$PKG/licenses/cygwin.CYGWIN_LICENSE"
cp "$WORK/cygsrc/newlib-cygwin/COPYING3.LIB" "$PKG/licenses/cygwin.LGPL-3.0.txt"
cp "$WORK/cygsrc/newlib-cygwin/COPYING3" "$PKG/licenses/gcc-libgcc.GPL-3.0.txt"
cp "$WORK/gcc-COPYING.RUNTIME-3.1" "$PKG/licenses/gcc-libgcc.GCC-Runtime-Library-Exception-3.1.txt"
grep -q 'Linking Exception' "$PKG/licenses/cygwin.CYGWIN_LICENSE" || { echo "CYGWIN_LICENSE has no linking exception" >&2; exit 1; }
grep -q 'GNU LESSER GENERAL PUBLIC LICENSE' "$PKG/licenses/cygwin.LGPL-3.0.txt" || { echo "COPYING3.LIB is not the LGPL" >&2; exit 1; }
grep -q 'GNU GENERAL PUBLIC LICENSE' "$PKG/licenses/gcc-libgcc.GPL-3.0.txt" || { echo "COPYING3 is not the GPL" >&2; exit 1; }
grep -q 'GCC RUNTIME LIBRARY EXCEPTION' "$PKG/licenses/gcc-libgcc.GCC-Runtime-Library-Exception-3.1.txt" || { echo "COPYING.RUNTIME is not the exception" >&2; exit 1; }
{
  write_notice_head
  cat << EOF_NOTICE

bin/cygwin1.dll is the Cygwin runtime $CYGWIN_VERSION. It is licensed under the GNU Lesser
General Public License, version 3 or later (cygwin.LGPL-3.0.txt). The Cygwin
linking exception that lets valkey-server.exe and valkey-cli.exe be used under
the Valkey licence is in cygwin.CYGWIN_LICENSE. The complete source of this exact
DLL is published next to this archive in the same release:
$CYGWIN_SRC

bin/cyggcc_s-seh-1.dll is libgcc $LIBGCC_VERSION of Cygwin (GCC runtime library). It is licensed under
the GNU General Public License, version 3 (gcc-libgcc.GPL-3.0.txt) with the GCC
Runtime Library Exception, version 3.1 (gcc-libgcc.GCC-Runtime-Library-Exception-3.1.txt).
EOF_NOTICE
} > "$PKG/licenses/NOTICE.txt"

{
  echo "valkey $VALKEY_VERSION for $PLATFORM"
  echo "built $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "commit ${GITHUB_SHA:-unknown}"
  echo "workflow run ${GITHUB_SERVER_URL:-}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}"
  echo "runner image ${ImageOS:-unknown} ${ImageVersion:-}"
  echo "system $(uname -srm)"
  echo "source: valkey-$VALKEY_VERSION.tar.gz sha256 $(sha256_of "$VALKEY_SRC_TARBALL") (unmodified)"
  echo "make BUILD_TLS=no OPTIMIZATION=-O2 CFLAGS=-Wno-char-subscripts valkey-server valkey-cli"
  echo "change to the build machine, not to Valkey: /usr/include/dlfcn.h, __GNU_VISIBLE replaced by 1"
  echo "cygwin $CYGWIN_VERSION (source: $CYGWIN_SRC sha256 $(sha256_of "$OUT/dist/$CYGWIN_SRC"))"
  echo "libgcc1 $LIBGCC_VERSION"
  echo "compiler: $(gcc --version | head -n 1)"
  echo "linker: $(ld --version | head -n 1)"
  echo "valkey-server.exe sha256 $(sha256_of "$PKG/bin/valkey-server.exe")"
  echo "valkey-cli.exe sha256 $(sha256_of "$PKG/bin/valkey-cli.exe")"
  echo "cygwin1.dll sha256 $(sha256_of "$PKG/bin/cygwin1.dll")"
  echo "cyggcc_s-seh-1.dll sha256 $(sha256_of "$PKG/bin/cyggcc_s-seh-1.dll")"
  echo "license: BSD-3-Clause (Valkey); cygwin1.dll LGPL-3.0+ with linking exception; cyggcc_s-seh-1.dll GPL-3.0 with GCC Runtime Library Exception; see licenses/"
} > "$PKG/BUILD-INFO.txt"

(cd "$PKG" && zip -q -r -X "$OUT/dist/$NAME" BUILD-INFO.txt bin licenses)
(cd "$OUT/dist" && printf '%s  %s\n' "$(sha256_of "$NAME")" "$NAME" > "$NAME.sha256")
unzip -l "$OUT/dist/$NAME" 2> /dev/null || true
cat "$OUT/dist/$NAME.sha256"
