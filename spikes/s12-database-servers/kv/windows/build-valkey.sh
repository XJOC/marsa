#!/usr/bin/env bash
# S-12b: build Valkey on Windows inside MSYS2 or Cygwin (the workflow starts this
# from the right shell). It records what happens and copies the result to $DIST.
#
#   build-valkey.sh msys2|cygwin
#
# Needs: gcc make curl tar. Valkey has no Windows port (no Cygwin or MSYS code in
# 8.1), so this tests the plain POSIX build with the one change redis-windows
# makes to Redis (a header guard in dlfcn.h), and only valkey-server and
# valkey-cli (valkey-benchmark does not compile: asprintf is hidden on MSYS2).
set -uo pipefail
FLAVOR="${1:?msys2|cygwin}"
V="${VALKEY_VERSION:-8.1.11}"
T="$(cygpath -u "${RUNNER_TEMP:?}")"
OUT="$T/kv-results"
DIST="$T/kv-dist"
WORK="$T/kv-build-$FLAVOR"
mkdir -p "$OUT" "$DIST" "$WORK"
fact() { printf '%s=%s\n' "$1" "$2" >>"$OUT/facts.txt"; echo "FACT $1=$2"; }

fact build_env "$(uname -srm)"
fact gcc "$(gcc --version | head -1)"
cd "$WORK" || exit 0
curl -fsSL -o "valkey-$V.tar.gz" "https://github.com/valkey-io/valkey/archive/refs/tags/$V.tar.gz" || { fact source "FAILED to fetch source"; exit 0; }
want="$(curl -fsSL https://raw.githubusercontent.com/valkey-io/valkey-hashes/main/README | awk -v f="valkey-$V.tar.gz" '$1=="hash" && $2==f {print $4}')"
got="$(sha256sum "valkey-$V.tar.gz" | cut -d' ' -f1)"
fact sha256_source_published_valkey_hashes "$want"
if [ "$want" = "$got" ]; then fact sha256_source_match yes; else fact sha256_source_match "NO (got $got)"; fi
tar xzf "valkey-$V.tar.gz"
cd "valkey-$V" || exit 0

if grep -q __GNU_VISIBLE /usr/include/dlfcn.h 2>/dev/null; then
  sed -i 's/__GNU_VISIBLE/1/' /usr/include/dlfcn.h
  fact patch_dlfcn_h "applied to /usr/include/dlfcn.h (as redis-windows does for MSYS2); without it Cygwin fails: Dl_info and dladdr are hidden behind __GNU_VISIBLE"
fi

built=no
attempt=0
# The default flags are -O3 plus LTO, which is slow here; start at -O2 (redis-windows uses -O0).
for opt in -O2 -O0; do
  attempt=$((attempt + 1))
  make distclean >/dev/null 2>&1 || true
  t0=$SECONDS
  make -j"$(nproc)" BUILD_TLS=no OPTIMIZATION="$opt" CFLAGS="-Wno-char-subscripts" valkey-server valkey-cli >"$OUT/build-$attempt.log" 2>&1; rc=$?
  fact "build_attempt_${attempt}_opt" "$opt"
  fact "build_attempt_${attempt}_rc" "$rc"
  fact "build_attempt_${attempt}_seconds" "$((SECONDS - t0))"
  if [ $rc -eq 0 ] && [ -x src/valkey-server.exe ]; then built="yes ($opt)"; break; fi
  fact "build_attempt_${attempt}_first_errors" "$(grep -E 'error:|undefined reference|\*\*\*' "$OUT/build-$attempt.log" | head -5 | cut -c1-240 | tr '\n' '|')"
done
fact built "$built"
[ "$built" = no ] && { fact source "own build FAILED, see build-N.log"; exit 0; }

cp src/valkey-server.exe src/valkey-cli.exe "$DIST/"
fact ldd "$(ldd src/valkey-server.exe | tr '\n' ';' | tr -s '\t ' ' ')"
# Copy the runtime DLLs that are not in C:\Windows (the Cygwin or MSYS runtime and friends).
ldd src/valkey-server.exe | awk '{print $3}' | grep -iE '/usr/bin|/bin/' | while read -r dll; do cp -n "$dll" "$DIST/" && echo "copied $dll"; done
fact dist_files "$(ls -la "$DIST" | tail -n +2 | awk '{print $9" "$5}' | tr '\n' ';')"
fact version "$("$DIST/valkey-server.exe" --version 2>&1 | head -1)"
