#!/usr/bin/env bash
# Packs the spc build in $WORK/buildroot into the archive recipes/php.toml
# unpacks (strip_components = 0, into = ""):
#
#   bin/php, bin/php-fpm      the two programs
#   ext/<name>.so             each shared extension (Xdebug)
#   licenses/                 spc's licence files for PHP, its extensions and
#                             libraries, and NOTICE.txt (the Xdebug acknowledgement)
#   BUILD-INFO.txt            the versions of every tool that made this build
#
#   WORK=/tmp/w OUT=/tmp/out PHP_VERSION=8.4.27 PLATFORM=darwin-arm64 package.sh
#
# Writes $OUT/pkg/ (the unpacked package, for smoke.sh), and in $OUT/dist/:
# php-<version>-<platform>.tar.gz and php-<version>-<platform>.tar.gz.sha256.
# PLATFORM is a Marsa platform key (darwin-arm64, darwin-x64, linux-x64, linux-arm64).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

: "${WORK:?WORK is required}" "${OUT:?OUT is required}" "${PHP_VERSION:?PHP_VERSION is required}" "${PLATFORM:?PLATFORM is required}"
case "$PLATFORM" in
  darwin-arm64 | darwin-x64 | linux-x64 | linux-arm64) ;;
  *) echo "unknown PLATFORM $PLATFORM" >&2; exit 1 ;;
esac
BR="$WORK/buildroot"
PKG="$OUT/pkg"
NAME="php-${PHP_VERSION}-${PLATFORM}.tar.gz"

rm -rf "$PKG"
mkdir -p "$PKG/bin" "$PKG/ext" "$PKG/licenses" "$OUT/dist"
cp "$BR/bin/php" "$BR/bin/php-fpm" "$PKG/bin/"
while IFS='|' read -r _ name; do
  cp "$BR/modules/$name.so" "$PKG/ext/"
done < <(shared_modules)
cp -R "$BR/license/." "$PKG/licenses/"
cat > "$PKG/licenses/NOTICE.txt" << 'EOF_NOTICE'
PHP for Marsa, built with static-php-cli (https://github.com/crazywhalecc/static-php-cli).

This folder holds the licence of PHP, of each extension, and of each library
built into bin/php and bin/php-fpm, as static-php-cli collected them.

This product includes Xdebug software, freely available from <https://xdebug.org/>.
The Xdebug licence is in ext_xdebug_0.txt.
EOF_NOTICE

compiler() {
  if [ -n "${SPC_TARGET:-}" ]; then
    echo "zig $(find "$WORK/pkgroot" -path '*/zig/zig' -type f -exec {} version \; | head -1) (target $SPC_TARGET)"
  else
    cc --version 2>&1 | head -1
  fi
}
{
  echo "php ${PHP_VERSION} for ${PLATFORM}"
  echo "built ${BUILD_DATE:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"
  echo "commit ${GITHUB_SHA:-unknown}"
  echo "workflow run ${GITHUB_SERVER_URL:-}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}"
  echo "runner image ${ImageOS:-unknown} ${ImageVersion:-unknown}"
  echo "system $(uname -s) $(uname -r) $(uname -m)"
  echo "static-php-cli ${SPC_VERSION:-unknown}"
  echo "compiler $(compiler)"
  if command -v xcodebuild > /dev/null; then echo "xcode $(xcodebuild -version | tr '\n' ' ')"; fi
  echo "xdebug ${XDEBUG_VERSION:-unknown}"
  echo
  echo "extensions (recipes/php-extensions.txt):"
  echo "  static: $(spc_names static)"
  echo "  shared: $(spc_names shared)"
  echo
  echo "sources spc downloaded:"
  ls -1 "$WORK/downloads" | sed 's/^/  /'
} > "$PKG/BUILD-INFO.txt"

chmod 755 "$PKG/bin/php" "$PKG/bin/php-fpm"
chmod 644 "$PKG"/ext/*.so "$PKG/BUILD-INFO.txt"
find "$PKG/licenses" -type f -exec chmod 644 {} +
# No macOS extended attributes or ._ files in the archive.
if command -v xattr > /dev/null; then xattr -cr "$PKG"; fi
COPYFILE_DISABLE=1 tar -czf "$OUT/dist/$NAME" -C "$PKG" BUILD-INFO.txt bin ext licenses
(cd "$OUT/dist" && printf '%s  %s\n' "$(sha256_of "$NAME")" "$NAME" > "$NAME.sha256")
cat "$OUT/dist/$NAME.sha256"
tar -tzvf "$OUT/dist/$NAME" > "$OUT/archive-list.txt"
cat "$OUT/archive-list.txt"
