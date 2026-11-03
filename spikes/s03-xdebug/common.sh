#!/usr/bin/env bash
# S-03 shared helpers. Sourced by the other scripts, not run directly.
#   source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
# Everything stays inside spikes/s03-xdebug/. Nothing is installed system-wide.
# Run order: 00-tools 10-fetch 12-path-a 15-inspect 30-build-xdebug-so 40-check 50-bench 60-patch-compat
# (about 10 minutes, ~1.2 GB in work/, which is git-ignored and safe to delete).

set -euo pipefail

S03_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$S03_DIR/work"
RESULTS="$S03_DIR/results"
DL="$WORK/downloads"
TOOLS="$WORK/tools"
PREFIX="$TOOLS/prefix"

mkdir -p "$WORK" "$RESULTS" "$DL" "$TOOLS" "$PREFIX"

# Build tools we compile ourselves come first on PATH. This only affects the
# running script, never the user's shell.
export PATH="$PREFIX/bin:$PATH"

# Versions pinned for this spike (also written in results/downloads.txt).
PHP_VERSION="${PHP_VERSION:-8.4.23}"
SPC_URL="https://dl.static-php.dev/v3/spc-bin/nightly/spc-macos-aarch64"
PHP_CLI_URL="https://dl.static-php.dev/static-php-cli/bulk/php-${PHP_VERSION}-cli-macos-aarch64.tar.gz"
PHP_FPM_URL="https://dl.static-php.dev/static-php-cli/bulk/php-${PHP_VERSION}-fpm-macos-aarch64.tar.gz"
PHP_SRC_URL="https://www.php.net/distributions/php-${PHP_VERSION}.tar.gz"
XDEBUG_VERSION="${XDEBUG_VERSION:-3.5.3}"
XDEBUG_URL="https://xdebug.org/files/xdebug-${XDEBUG_VERSION}.tgz"

# fetch URL [FILENAME]: download into work/downloads and log url + sha256 + size.
fetch() {
  local url="$1" name="${2:-$(basename "$1")}"
  local dest="$DL/$name"
  if [ ! -s "$dest" ]; then
    echo "download: $url"
    curl -fsSL --retry 3 --connect-timeout 20 -o "$dest.part" "$url"
    mv "$dest.part" "$dest"
  fi
  local sum size
  sum="$(shasum -a 256 "$dest" | awk '{print $1}')"
  size="$(stat -f %z "$dest")"
  touch "$RESULTS/downloads.txt"
  if ! grep -q " $url\$" "$RESULTS/downloads.txt"; then
    printf '%s  %s bytes  %s  %s\n' "$sum" "$size" "$(date -u +%Y-%m-%dT%H:%MZ)" "$url" >> "$RESULTS/downloads.txt"
  fi
}

# need_disk_gb N: stop when free space on / is below N GB (other agents build too).
need_disk_gb() {
  local want="$1" free
  free="$(df -g / | awk 'NR==2 {print $4}')"
  echo "free disk: ${free} GB (need ${want} GB)"
  if [ "$free" -lt "$want" ]; then
    echo "STOP: free disk below ${want} GB" >&2
    exit 3
  fi
}
