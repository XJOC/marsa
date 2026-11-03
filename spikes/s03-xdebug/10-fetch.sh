#!/usr/bin/env bash
# S-03 step 2: download spc (v3 nightly), the prebuilt PHP 8.4 cli + fpm from
# the static-php-cli "bulk" set, Xdebug source, and the matching PHP source (for headers).
# Everything goes to work/downloads/ and is logged in results/downloads.txt (url + sha256).
# Run:  ./10-fetch.sh        (about 2 minutes, ~110 MB)
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

need_disk_gb 8

fetch "$SPC_URL" spc-macos-aarch64
chmod +x "$DL/spc-macos-aarch64"
fetch "$PHP_CLI_URL"
fetch "$PHP_FPM_URL"
fetch "$XDEBUG_URL"
fetch "$PHP_SRC_URL"

# Unpack. Prebuilt archives hold a single file (php / php-fpm).
for kind in cli fpm; do
  d="$WORK/php-$kind"
  if [ ! -d "$d" ]; then
    mkdir -p "$d"
    tar -xzf "$DL/php-${PHP_VERSION}-$kind-macos-aarch64.tar.gz" -C "$d"
  fi
done
[ -d "$WORK/src/xdebug-$XDEBUG_VERSION" ] || { mkdir -p "$WORK/src"; tar -xzf "$DL/xdebug-$XDEBUG_VERSION.tgz" -C "$WORK/src"; }
[ -d "$WORK/src/php-$PHP_VERSION" ] || { mkdir -p "$WORK/src"; tar -xzf "$DL/php-$PHP_VERSION.tar.gz" -C "$WORK/src"; }

# Record the spc version.
{ echo "# spc version (from $SPC_URL)"; "$DL/spc-macos-aarch64" --version 2>&1 | head -3; } > "$RESULTS/spc-version.txt"
cat "$RESULTS/spc-version.txt"
ls -l "$WORK/php-cli" "$WORK/php-fpm"
