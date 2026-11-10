#!/usr/bin/env bash
# shellcheck disable=SC2317  # `check` runs the small functions below by name
# Checks one platform's archive as it ships: unpacks it into a new folder and
# runs the programs from there, on the system they were built for.
#
#   VERSION=0.1.0 PLATFORM=darwin-arm64 OUT=/tmp/out smoke.sh
#
# Reads $OUT/dist/<archive> (from package.sh). Writes $OUT/smoke.md, one line
# per check, and $OUT/smoke-status.txt ("passed" or "FAILED").
set -euo pipefail
# shellcheck source=tools/release/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

: "${PLATFORM:?PLATFORM is required}" "${OUT:?OUT is required}"
set_platform "$PLATFORM"
DIR="$OUT/unpacked"
failed=0
: > "$OUT/smoke.md"

# check <what> <command...>: runs the command and records "pass" or "FAIL".
check() {
  local what="$1"
  shift
  if "$@"; then
    echo "- pass: $what" | tee -a "$OUT/smoke.md"
  else
    echo "- FAIL: $what" | tee -a "$OUT/smoke.md"
    failed=1
  fi
}

rm -rf "$DIR"
mkdir -p "$DIR"
case "$ARCHIVE" in
  *.zip) 7z x -bso0 "-o$DIR" "$OUT/dist/$ARCHIVE" ;;
  *) tar -xzf "$OUT/dist/$ARCHIVE" -C "$DIR" ;;
esac

# The archive holds the required programs, maybe the optional ones, the two
# text files, and nothing else.
can_run() { [ -f "$DIR/$1" ] && [ -x "$DIR/$1" ]; }
not_empty() { [ -s "$DIR/$1" ]; }
allowed=(LICENSE BUILD-INFO.txt)
for name in "${PROGRAMS[@]}"; do
  allowed+=("$name$EXE")
  check "$name$EXE is in the archive and can run" can_run "$name$EXE"
done
for name in "${OPTIONAL_PROGRAMS[@]}"; do allowed+=("$name$EXE"); done
check "LICENSE is in the archive" not_empty LICENSE
check "BUILD-INFO.txt is in the archive" not_empty BUILD-INFO.txt
only_allowed() {
  local file
  for file in "$DIR"/* "$DIR"/.[!.]*; do
    [ -e "$file" ] || continue
    [[ " ${allowed[*]} " == *" $(basename "$file") "* ]] || { echo "unexpected file: $file" >&2; return 1; }
  done
}
check "nothing else is in the archive" only_allowed

# prints <program> <argument> <line>: the program's whole output is that one
# line. `tr` drops the \r that Windows programs add.
prints() { [ "$("$DIR/$1$EXE" "$2" | tr -d '\r')" = "$3" ]; }
check "marsa --version prints 'marsa $VERSION'" prints marsa --version "marsa $VERSION"
check "marsad --version prints 'marsad $VERSION'" prints marsad --version "marsad $VERSION"
check "marsa-helper version prints '$VERSION'" prints marsa-helper version "$VERSION"
# Under its own name the shim has no job: it must say so and exit with 127.
# That still proves the program starts on this system.
shim_starts() {
  local output status=0
  output="$("$DIR/marsa-shim$EXE" 2>&1)" || status=$?
  [ "$status" -eq 127 ] && [[ "$output" == *"[shim_unknown_name]"* ]]
}
check "marsa-shim starts, and refuses to run under its own name" shim_starts

if [[ "$PLATFORM" == darwin-* ]]; then
  for file in "$DIR"/marsa*; do
    check "$(basename "$file") is ad-hoc signed" is_adhoc_signed "$file"
  done
fi

if [ "$failed" -eq 0 ]; then echo passed > "$OUT/smoke-status.txt"; else echo FAILED > "$OUT/smoke-status.txt"; fi
echo "Smoke tests: $(cat "$OUT/smoke-status.txt")"
exit "$failed"
