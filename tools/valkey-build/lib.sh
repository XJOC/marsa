#!/usr/bin/env bash
# Shared by build.sh and build-cygwin.sh. Sourced, not run.

PINNED="${PINNED:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/downloads.sha256}"

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

# Downloads a URL to <dir>/<name> and checks it against downloads.sha256.
fetch_pinned() {
  local url="$1" dest="$2"
  fetch "$url" "$dest"
  check_pinned "$dest"
}

# Downloads the Valkey source named by VALKEY_VERSION into <work>, checks its
# pinned hash, and unpacks it into <work>/src. Sets VALKEY_SRC_TARBALL (the
# file) and VALKEY_SRC (the source folder).
fetch_valkey_source() {
  local work="$1"
  VALKEY_SRC_TARBALL="$work/valkey-$VALKEY_VERSION.tar.gz"
  fetch_pinned "https://github.com/valkey-io/valkey/archive/refs/tags/$VALKEY_VERSION.tar.gz" "$VALKEY_SRC_TARBALL"
  mkdir -p "$work/src"
  tar -xzf "$VALKEY_SRC_TARBALL" -C "$work/src" --strip-components=1
  VALKEY_SRC="$work/src"
  [ -f "$VALKEY_SRC/COPYING" ] || { echo "the Valkey tarball has no COPYING" >&2; exit 1; }
}

# Copies the licence of Valkey and of the code it bundles (the parts that are
# compiled into valkey-server and valkey-cli with MALLOC=libc: hiredis, Lua,
# linenoise, hdr_histogram, fpconv) into <dest>.
collect_valkey_licenses() {
  local src="$1" dest="$2" n
  mkdir -p "$dest"
  cp "$src/COPYING" "$dest/COPYING"
  cp "$src/deps/hiredis/COPYING" "$dest/hiredis.COPYING"
  cp "$src/deps/lua/COPYRIGHT" "$dest/lua.COPYRIGHT"
  cp "$src/deps/hdr_histogram/COPYING.txt" "$dest/hdr_histogram.COPYING.txt"
  cp "$src/deps/hdr_histogram/LICENSE.txt" "$dest/hdr_histogram.LICENSE.txt"
  cp "$src/deps/fpconv/LICENSE.txt" "$dest/fpconv.LICENSE.txt"
  # linenoise keeps its licence in the comment at the top of its source file,
  # up to the disclaimer's last line.
  awk '{ print } /EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE/ { exit }' "$src/deps/linenoise/linenoise.c" > "$dest/linenoise.LICENSE"
  grep -q 'Redistribution and use in source and binary forms' "$dest/linenoise.LICENSE" ||
    { echo "no licence text found at the top of linenoise.c" >&2; exit 1; }
  grep -q 'BSD 3-Clause License' "$dest/COPYING" || { echo "COPYING is not the BSD licence we expect" >&2; exit 1; }
  for n in "$dest"/*; do [ -s "$n" ] || { echo "empty licence file: $n" >&2; exit 1; }; done
}

# The first lines of licenses/NOTICE.txt, the same on every platform.
write_notice_head() {
  cat << 'EOF_NOTICE'
Valkey for Marsa. Valkey is BSD-3-Clause licensed (COPYING, which also holds the
licence of the Redis code it descends from). The server and the command-line
client contain code from these bundled projects, each with its licence file here:

  hiredis          hiredis.COPYING
  Lua 5.1          lua.COPYRIGHT
  linenoise        linenoise.LICENSE
  hdr_histogram    hdr_histogram.COPYING.txt, hdr_histogram.LICENSE.txt
  fpconv           fpconv.LICENSE.txt

This build uses the C library's allocator (MALLOC=libc) and no TLS, so jemalloc,
fast_float and OpenSSL are not part of it.
EOF_NOTICE
}

# Number of CPUs, for make -j.
ncpu() { nproc 2> /dev/null || sysctl -n hw.ncpu; }
