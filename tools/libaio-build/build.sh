#!/usr/bin/env bash
# Builds libaio.so.1 and libnuma.so.1 from the pinned upstream sources, for the
# machine it runs on. Meant to run as root inside an almalinux:8 container
# (glibc 2.28), so the libraries run on every glibc distribution from 2.28 on.
# Marsa ships them next to MySQL because Ubuntu 24.04 no longer installs
# libaio1, and Oracle's mysqld needs libaio.so.1 and libnuma.so.1 (its archive
# bundles OpenSSL, protobuf and ICU, but not these two).
#
#   LIBAIO_VERSION=0.3.113 NUMACTL_VERSION=2.0.19 build.sh <output dir>
#
# The output dir gets libaio.so.1 and libnuma.so.1 (stripped; each SONAME is
# its file name), libaio-<version>.tar.gz and numactl_<version>.orig.tar.gz
# (the exact sources), COPYING (libaio, LGPL 2.1), COPYING.numactl (libnuma,
# LGPL 2.1), and BUILD-INFO.txt. The script fails if a library needs a glibc
# newer than 2.28, a library other than libc, or has another SONAME.
#
# Needs: gcc, make, binutils, file, curl, tar, gzip, coreutils, and for numactl
# autoconf, automake, libtool.
set -euo pipefail
export LC_ALL=C

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PINNED="$HERE/downloads.sha256"
MAX_GLIBC="2.28"

: "${LIBAIO_VERSION:?LIBAIO_VERSION is required}"
OUT_ARG="${1:?usage: build.sh <output dir>}"

: "${NUMACTL_VERSION:?NUMACTL_VERSION is required}"

[[ "$LIBAIO_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "LIBAIO_VERSION must look like 0.3.113, got: $LIBAIO_VERSION" >&2
  exit 1
}
[[ "$NUMACTL_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "NUMACTL_VERSION must look like 2.0.19, got: $NUMACTL_VERSION" >&2
  exit 1
}

# sha256 of a file, as 64 lowercase hex characters.
sha256_of() { (sha256sum "$1" 2> /dev/null || shasum -a 256 "$1") | cut -d' ' -f1; }

# Stops unless the file's sha256 is the one listed for its name in downloads.sha256.
check_pinned() {
  local file="$1" name want got
  name="$(basename "$file")"
  want="$(awk -v n="$name" '$1 !~ /^#/ && $2 == n { print $1 }' "$PINNED")"
  got="$(sha256_of "$file")"
  if [ -z "$want" ]; then
    echo "$name is not listed in $PINNED" >&2
    exit 1
  fi
  if [ "$want" != "$got" ]; then
    echo "$name: sha256 is $got, but $PINNED says $want" >&2
    exit 1
  fi
  echo "sha256 ok: $name $got"
}

# Downloads a URL to a file, with retries. HTTPS only, also across redirects.
fetch() { curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 --retry-delay 5 -o "$2" "$1"; }

# Checks a built library: SONAME equals its file name, only libc and the loader
# are NEEDED, and no symbol asks for a glibc newer than $MAX_GLIBC. Sets
# CHECKED_GLIBC to the highest GLIBC_ version used ("none" if no symbol has one).
check_so() {
  local so="$1" want_soname="$2" dynamic soname needed lib glibc_versions highest
  echo "--- $want_soname: file"
  file -L "$so"
  echo "--- $want_soname: dynamic section (SONAME, NEEDED)"
  dynamic="$(readelf -d "$so")"
  printf '%s\n' "$dynamic" | grep -E 'SONAME|NEEDED' || true

  soname="$(printf '%s\n' "$dynamic" | sed -n 's/.*(SONAME).*\[\(.*\)\]/\1/p')"
  if [ "$soname" != "$want_soname" ]; then
    echo "SONAME is '$soname', expected $want_soname" >&2
    exit 1
  fi

  # Only libc and the dynamic loader are allowed.
  needed="$(printf '%s\n' "$dynamic" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')"
  while IFS= read -r lib; do
    [ -n "$lib" ] || continue
    case "$lib" in
      libc.so.6 | ld-linux-*.so.* | ld-linux.so.*) ;;
      *) echo "$want_soname: unexpected NEEDED library: $lib" >&2; exit 1 ;;
    esac
  done <<< "$needed"

  echo "--- $want_soname: ldd"
  ldd "$so" || true

  # The highest GLIBC_x.y version any symbol asks for. Zero is fine: libaio does
  # its work with raw system calls.
  glibc_versions="$(readelf -W --dyn-syms "$so" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -uV || true)"
  if [ -z "$glibc_versions" ]; then
    CHECKED_GLIBC="none"
  else
    CHECKED_GLIBC="$(printf '%s\n' "$glibc_versions" | tail -n 1)"
    highest="$(printf '%s\n%s\n' "$CHECKED_GLIBC" "$MAX_GLIBC" | sort -V | tail -n 1)"
    if [ "$highest" != "$MAX_GLIBC" ]; then
      echo "$want_soname needs GLIBC_$CHECKED_GLIBC, newer than GLIBC_$MAX_GLIBC" >&2
      exit 1
    fi
  fi
  echo "--- $want_soname: highest GLIBC_ symbol version used: $CHECKED_GLIBC (limit $MAX_GLIBC)"
}

if [ -n "${EXPECT_ARCH:-}" ] && [ "$(uname -m)" != "$EXPECT_ARCH" ]; then
  echo "this machine is $(uname -m), expected $EXPECT_ARCH" >&2
  exit 1
fi

mkdir -p "$OUT_ARG"
OUT="$(cd "$OUT_ARG" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

tarball="libaio-$LIBAIO_VERSION.tar.gz"

# Upstream publishes the tarball on Codeberg. Debian's orig tarball is the same
# file (same sha256); it is the fallback if Codeberg is down. Either way the
# pinned hash decides.
fetch "https://codeberg.org/jmoyer/libaio/releases/download/libaio-$LIBAIO_VERSION/$tarball" "$WORK/$tarball" ||
  fetch "https://deb.debian.org/debian/pool/main/liba/libaio/libaio_$LIBAIO_VERSION.orig.tar.gz" "$WORK/$tarball"
check_pinned "$WORK/$tarball"

mkdir "$WORK/src"
tar -xzf "$WORK/$tarball" -C "$WORK/src" --strip-components=1
[ -f "$WORK/src/COPYING" ] || { echo "the tarball has no COPYING" >&2; exit 1; }
grep -q 'GNU LESSER GENERAL PUBLIC LICENSE' "$WORK/src/COPYING" || { echo "COPYING is not the LGPL" >&2; exit 1; }
grep -q 'Version 2.1' "$WORK/src/COPYING" || { echo "COPYING is not LGPL version 2.1" >&2; exit 1; }

# CFLAGS comes from the environment, not from `make CFLAGS=...`: the Makefile
# adds -fPIC with `+=`, and a value on the make command line would drop it.
# ENABLE_SHARED=1 is the default; it is explicit so a Makefile change shows up.
(
  cd "$WORK/src"
  CFLAGS="-O2 -fomit-frame-pointer" make -C src ENABLE_SHARED=1
)

# The Makefile names the real file libaio.so.1.<minor>.<micro>; its SONAME is
# libaio.so.1, and that is the name mysqld asks for.
shopt -s nullglob
built=("$WORK"/src/src/libaio.so.1.*)
shopt -u nullglob
[ "${#built[@]}" -eq 1 ] || { echo "expected one libaio.so.1.* in src/, found ${#built[@]}" >&2; exit 1; }
so="$WORK/libaio.so.1"
cp "${built[0]}" "$so"
strip --strip-unneeded "$so"

check_so "$so" libaio.so.1
libaio_glibc="$CHECKED_GLIBC"

# --- libnuma, from numactl
# Debian's orig tarball is the same file as GitHub's tag archive; the pinned
# hash decides which one we got. It has no Makefile.in or libtool files, so
# autoreconf makes them (the release asset with a ready configure has no
# published checksum, see downloads.sha256).
numa_tarball="numactl_$NUMACTL_VERSION.orig.tar.gz"
fetch "https://deb.debian.org/debian/pool/main/n/numactl/$numa_tarball" "$WORK/$numa_tarball" ||
  fetch "https://github.com/numactl/numactl/archive/refs/tags/v$NUMACTL_VERSION.tar.gz" "$WORK/$numa_tarball"
check_pinned "$WORK/$numa_tarball"

mkdir "$WORK/numa"
tar -xzf "$WORK/$numa_tarball" -C "$WORK/numa" --strip-components=1
numa_license="$WORK/numa/LICENSE.LGPL2.1"
[ -f "$numa_license" ] || { echo "the numactl tarball has no LICENSE.LGPL2.1" >&2; exit 1; }
grep -q 'GNU LESSER GENERAL PUBLIC LICENSE' "$numa_license" || { echo "LICENSE.LGPL2.1 is not the LGPL" >&2; exit 1; }
grep -q 'Version 2.1' "$numa_license" || { echo "LICENSE.LGPL2.1 is not LGPL version 2.1" >&2; exit 1; }
# libnuma is the LGPL part of numactl; the tools (numactl, numastat, ...) are GPL 2.
grep -q '`libnuma` is under the GNU Lesser General Public License, v2.1' "$WORK/numa/README.md" ||
  { echo "numactl's README no longer says libnuma is LGPL 2.1: read the license again" >&2; exit 1; }

# Only the library is built (`make libnuma.la`), nothing is installed. The
# version in configure.ac must be the one we pinned.
(
  cd "$WORK/numa"
  [ "$(tr -d '\n' < VERSION)" = "$NUMACTL_VERSION" ] || { echo "the numactl source is not version $NUMACTL_VERSION" >&2; exit 1; }
  autoreconf --install --symlink --force
  CFLAGS="-O2 -fomit-frame-pointer" ./configure --disable-static
  make libnuma.la
)

# libtool names the real file libnuma.so.1.<age>.<revision> and links
# libnuma.so.1 and libnuma.so to it. Take the real file.
shopt -s nullglob
numa_built=("$WORK"/numa/.libs/libnuma.so.1.*)
shopt -u nullglob
[ "${#numa_built[@]}" -eq 1 ] || { echo "expected one libnuma.so.1.* in .libs/, found ${#numa_built[@]}" >&2; exit 1; }
[ -f "${numa_built[0]}" ] && [ ! -L "${numa_built[0]}" ] || { echo "${numa_built[0]} is not a regular file" >&2; exit 1; }
numa_so="$WORK/libnuma.so.1"
cp "${numa_built[0]}" "$numa_so"
strip --strip-unneeded "$numa_so"
check_so "$numa_so" libnuma.so.1
libnuma_glibc="$CHECKED_GLIBC"

cp "$so" "$OUT/libaio.so.1"
cp "$numa_so" "$OUT/libnuma.so.1"
cp "$WORK/$tarball" "$OUT/$tarball"
cp "$WORK/$numa_tarball" "$OUT/$numa_tarball"
cp "$WORK/src/COPYING" "$OUT/COPYING"
cp "$numa_license" "$OUT/COPYING.numactl"
{
  echo "libaio $LIBAIO_VERSION, built for $(uname -m)"
  echo "source: $tarball sha256 $(sha256_of "$OUT/$tarball")"
  echo "libaio.so.1 sha256 $(sha256_of "$OUT/libaio.so.1")"
  echo "libaio highest GLIBC_ version used: $libaio_glibc (limit $MAX_GLIBC)"
  echo "numactl $NUMACTL_VERSION (libnuma)"
  echo "source: $numa_tarball sha256 $(sha256_of "$OUT/$numa_tarball")"
  echo "libnuma.so.1 sha256 $(sha256_of "$OUT/libnuma.so.1")"
  echo "libnuma highest GLIBC_ version used: $libnuma_glibc (limit $MAX_GLIBC)"
  echo "build glibc: $(getconf GNU_LIBC_VERSION)"
  echo "compiler: $(gcc --version | head -n 1)"
  echo "linker: $(ld --version | head -n 1)"
  echo "autotools: $(autoconf --version | head -n 1); $(automake --version | head -n 1); $(libtoolize --version | head -n 1)"
  echo "license: libaio LGPL-2.1 (COPYING); libnuma LGPL-2.1 (COPYING.numactl, the library part of numactl; its tools are GPL-2);"
  echo "  the sources are the tarballs above, built unmodified"
} > "$OUT/BUILD-INFO.txt"
# The caller (outside the container) must be able to read and rename these.
chmod -R a+rX "$OUT"
echo "--- output"
ls -l "$OUT"
cat "$OUT/BUILD-INFO.txt"
