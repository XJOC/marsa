#!/usr/bin/env bash
# shellcheck disable=SC2034  # the scripts that source this file use its variables
# Helpers shared by the release scripts. Source it; do not run it.
#
# After `set_platform <platform>`:
#   EXE       ".exe" on Windows, empty elsewhere
#   ARCHIVE   marsa-<version>-<platform>.tar.gz, or .zip on Windows

# The repository root.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# One sort order everywhere, so the file lists and SHA256SUMS never change order.
export LC_ALL=C

# Every platform Marsa is released for (the keys of recipes/, plus Windows).
PLATFORMS=(darwin-arm64 darwin-x64 linux-x64 linux-arm64 windows-x64)

# The programs every archive must hold.
PROGRAMS=(marsa marsad marsa-shim marsa-helper marsa-gui)
# The tray icon is a second program of marsa-gui (task M5-04). It ships when
# the build made it.
OPTIONAL_PROGRAMS=(marsa-tray)

set_platform() {
  : "${VERSION:?VERSION is required}"
  case "$1" in
    darwin-arm64 | darwin-x64 | linux-x64 | linux-arm64) EXE="" ARCHIVE="marsa-$VERSION-$1.tar.gz" ;;
    windows-x64) EXE=".exe" ARCHIVE="marsa-$VERSION-$1.zip" ;;
    *) echo "unknown platform $1" >&2; exit 1 ;;
  esac
}

# sha256 of a file, as 64 lowercase hex characters.
sha256_of() { (sha256sum "$1" 2> /dev/null || shasum -a 256 "$1") | cut -d' ' -f1; }

# Succeeds if the program has a valid ad-hoc signature (macOS). The output of
# codesign is saved first: `grep -q` would close the pipe early, and the
# SIGPIPE would look like "no match" under `pipefail`.
is_adhoc_signed() {
  local info
  codesign --verify --strict "$1" || return 1
  info="$(codesign --display --verbose=2 "$1" 2>&1)" || return 1
  grep -qx 'Signature=adhoc' <<< "$info"
}

# The names inside an archive, one per line, sorted.
archive_names() {
  case "$1" in
    *.zip) unzip -Z1 "$1" | tr -d '\r' | sort ;;
    *) tar -tzf "$1" | sort ;;
  esac
}
