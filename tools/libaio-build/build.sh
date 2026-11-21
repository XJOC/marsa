#!/usr/bin/env bash
# Builds libaio.so.1 from the pinned upstream source, for the machine it runs
# on. Meant to run as root inside an almalinux:8 container (glibc 2.28), so the
# library runs on every glibc distribution from 2.28 on. Marsa ships it next to
# MySQL because Ubuntu 24.04 no longer installs libaio1 and mysqld needs it.
#
#   LIBAIO_VERSION=0.3.113 build.sh <output dir>
#
# The output dir gets libaio.so.1 (stripped; its SONAME is libaio.so.1),
# libaio-<version>.tar.gz (the exact source, LGPL 2.1), COPYING, and
# BUILD-INFO.txt. The script fails if the library needs a glibc newer than
# 2.28, a library other than libc, or has another SONAME.
#
# Needs: gcc, make, binutils, file, curl, tar, gzip, coreutils.
set -euo pipefail
export LC_ALL=C

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PINNED="$HERE/downloads.sha256"
MAX_GLIBC="2.28"

: "${LIBAIO_VERSION:?LIBAIO_VERSION is required}"
OUT_ARG="${1:?usage: build.sh <output dir>}"

[[ "$LIBAIO_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "LIBAIO_VERSION must look like 0.3.113, got: $LIBAIO_VERSION" >&2
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

echo "--- file"
file -L "$so"
echo "--- dynamic section (SONAME, NEEDED)"
dynamic="$(readelf -d "$so")"
printf '%s\n' "$dynamic" | grep -E 'SONAME|NEEDED' || true

soname="$(printf '%s\n' "$dynamic" | sed -n 's/.*(SONAME).*\[\(.*\)\]/\1/p')"
if [ "$soname" != "libaio.so.1" ]; then
  echo "SONAME is '$soname', expected libaio.so.1" >&2
  exit 1
fi

# Only libc and the dynamic loader are allowed.
needed="$(printf '%s\n' "$dynamic" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')"
while IFS= read -r lib; do
  [ -n "$lib" ] || continue
  case "$lib" in
    libc.so.6 | ld-linux-*.so.* | ld-linux.so.*) ;;
    *) echo "unexpected NEEDED library: $lib" >&2; exit 1 ;;
  esac
done <<< "$needed"

echo "--- ldd"
ldd "$so" || true

# The highest GLIBC_x.y version any symbol asks for. Zero is fine: libaio does
# its work with raw system calls.
glibc_versions="$(readelf -W --dyn-syms "$so" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -uV || true)"
if [ -z "$glibc_versions" ]; then
  max_used="none"
else
  max_used="$(printf '%s\n' "$glibc_versions" | tail -n 1)"
  highest="$(printf '%s\n%s\n' "$max_used" "$MAX_GLIBC" | sort -V | tail -n 1)"
  if [ "$highest" != "$MAX_GLIBC" ]; then
    echo "libaio.so.1 needs GLIBC_$max_used, newer than GLIBC_$MAX_GLIBC" >&2
    exit 1
  fi
fi
echo "--- highest GLIBC_ symbol version used: $max_used (limit $MAX_GLIBC)"

cp "$so" "$OUT/libaio.so.1"
cp "$WORK/$tarball" "$OUT/$tarball"
cp "$WORK/src/COPYING" "$OUT/COPYING"
{
  echo "libaio $LIBAIO_VERSION, built for $(uname -m)"
  echo "source: $tarball sha256 $(sha256_of "$OUT/$tarball")"
  echo "libaio.so.1 sha256 $(sha256_of "$OUT/libaio.so.1")"
  echo "highest GLIBC_ version used: $max_used (limit $MAX_GLIBC)"
  echo "build glibc: $(getconf GNU_LIBC_VERSION)"
  echo "compiler: $(gcc --version | head -n 1)"
  echo "linker: $(ld --version | head -n 1)"
  echo "license: LGPL-2.1 (COPYING); the source is the tarball above, built unmodified"
} > "$OUT/BUILD-INFO.txt"
# The caller (outside the container) must be able to read and rename these.
chmod -R a+rX "$OUT"
echo "--- output"
ls -l "$OUT"
cat "$OUT/BUILD-INFO.txt"
