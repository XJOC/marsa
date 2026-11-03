#!/usr/bin/env bash
# S-03 step 1: build GNU m4 and autoconf from source into work/tools/prefix.
# Why: phpize needs autoconf, and this Mac has none. No Homebrew, no sudo.
# Run:  ./00-tools.sh        (about 3 minutes)
# Output: work/tools/prefix/bin/{m4,autoconf,autoheader,autoreconf,...}
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

need_disk_gb 8
JOBS="$(sysctl -n hw.ncpu)"

build_gnu() {   # build_gnu NAME VERSION  (downloads NAME-VERSION.tar.gz from GNU)
  local name="$1" ver="$2" src="$TOOLS/src"
  mkdir -p "$src"
  fetch "https://ftpmirror.gnu.org/gnu/$name/$name-$ver.tar.gz"
  if [ -x "$PREFIX/bin/$name" ]; then echo "$name already built"; return; fi
  rm -rf "$src/$name-$ver"
  tar -xzf "$DL/$name-$ver.tar.gz" -C "$src"
  ( cd "$src/$name-$ver" && ./configure --prefix="$PREFIX" >/dev/null && make -j"$JOBS" >/dev/null && make install >/dev/null )
}

build_gnu m4 1.4.19
build_gnu autoconf 2.71

m4 --version | head -1
autoconf --version | head -1
