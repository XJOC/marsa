#!/bin/bash
# Downloads the official Caddy (darwin-arm64) into ../bin and checks it against the
# release's checksums file. Installs nothing outside this folder. No sudo.
# CADDY_BASE_URL can point at a mirror (the checksum failure path is tested with a
# tampered copy).
set -eu
CADDY_VERSION=2.11.7
BASE=${CADDY_BASE_URL:-https://github.com/caddyserver/caddy/releases/download/v$CADDY_VERSION}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=$ROOT/bin
DL=$BIN/.download
ARCHIVE=caddy_${CADDY_VERSION}_mac_arm64.tar.gz
SUMS=caddy_${CADDY_VERSION}_checksums.txt

rm -rf "$DL"; mkdir -p "$DL/extract"
curl -fsSL --max-time 120 -o "$DL/$SUMS" "$BASE/$SUMS"
curl -fsSL --max-time 300 -o "$DL/$ARCHIVE" "$BASE/$ARCHIVE"

# The algorithm follows the hash length in the file: 64 hex = sha256, 128 = sha512.
expected=$(awk -v f="$ARCHIVE" '$2 == f {print $1}' "$DL/$SUMS")
case ${#expected} in
  64) alg=256 ;;
  128) alg=512 ;;
  *) echo "FAIL: no usable checksum line for $ARCHIVE in $SUMS" >&2; rm -rf "$DL"; exit 1 ;;
esac
actual=$(shasum -a "$alg" "$DL/$ARCHIVE" | awk '{print $1}')
if [ "$expected" != "$actual" ]; then
  echo "FAIL: sha$alg mismatch for $ARCHIVE" >&2
  echo "  expected $expected" >&2
  echo "  actual   $actual" >&2
  rm -rf "$DL"; exit 1
fi
echo "sha$alg OK for $ARCHIVE"

tar -xzf "$DL/$ARCHIVE" -C "$DL/extract" caddy
mv "$DL/extract/caddy" "$BIN/caddy"
echo "archive sha256: $(shasum -a 256 "$DL/$ARCHIVE" | awk '{print $1}')"
rm -rf "$DL"

echo "binary sha256:  $(shasum -a 256 "$BIN/caddy" | awk '{print $1}')"
"$BIN/caddy" version
file "$BIN/caddy"
codesign -dv "$BIN/caddy" 2>&1 || true
echo "xattr -l (empty means no quarantine):"; xattr -l "$BIN/caddy"
