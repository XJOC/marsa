#!/usr/bin/env bash
# Packs one platform's release build of Marsa into the archive users download:
#
#   marsa, marsad, marsa-shim, marsa-helper, marsa-gui   the programs
#   marsa-tray                                            if the build made it
#   LICENSE
#   BUILD-INFO.txt                                        what built these files
#
# All in one flat folder: `marsa` looks for `marsad` next to itself, and
# `marsad` looks for `marsa-helper` next to itself. The install script puts
# them in <app data>/bin/ (architecture.md section 7).
#
#   VERSION=0.1.0 PLATFORM=darwin-arm64 TRIPLE=aarch64-apple-darwin OUT=/tmp/out package.sh
#
# Reads the programs from BIN_DIR (default: target/$TRIPLE/release). Writes
# $OUT/pkg/ (the unpacked package) and, in $OUT/dist/: the archive, its
# .sha256, and BUILD-INFO-<platform>.txt (a copy, for the release notes).
set -euo pipefail
# shellcheck source=tools/release/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

: "${PLATFORM:?PLATFORM is required}" "${TRIPLE:?TRIPLE is required}" "${OUT:?OUT is required}"
set_platform "$PLATFORM"
BIN_DIR="${BIN_DIR:-$ROOT/target/$TRIPLE/release}"
PKG="$OUT/pkg"

programs=("${PROGRAMS[@]}")
for name in "${OPTIONAL_PROGRAMS[@]}"; do
  if [ -f "$BIN_DIR/$name$EXE" ]; then programs+=("$name"); fi
done

rm -rf "$PKG"
mkdir -p "$PKG" "$OUT/dist"
files=()
for name in "${programs[@]}"; do
  [ -f "$BIN_DIR/$name$EXE" ] || { echo "missing $BIN_DIR/$name$EXE: build it first" >&2; exit 1; }
  cp "$BIN_DIR/$name$EXE" "$PKG/"
  chmod 755 "$PKG/$name$EXE"
  files+=("$name$EXE")
done
cp "$ROOT/LICENSE" "$PKG/LICENSE"

# macOS: an ad-hoc signature on every program (ADR 0004). Apple Silicon runs no
# arm64 code without one. It is not notarization: Apple has not seen the files.
# The identifier is fixed, so it does not change with every build.
if [[ "$PLATFORM" == darwin-* ]]; then
  for file in "${files[@]}"; do
    codesign --sign - --force --identifier "dev.marsa.$file" "$PKG/$file"
    is_adhoc_signed "$PKG/$file" || { echo "$file is not ad-hoc signed" >&2; exit 1; }
  done
fi

# The newest GLIBC_x.y symbol version a program asks for: it starts only on a
# system whose glibc is at least that.
glibc_of() { objdump -T "$1" | sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' | sort -uV | tail -n 1; }

{
  echo "marsa $VERSION for $PLATFORM ($TRIPLE)"
  echo "built ${BUILD_DATE:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"
  echo "commit ${GITHUB_SHA:-unknown}"
  echo "workflow run ${GITHUB_SERVER_URL:-}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}"
  echo "runner image ${ImageOS:-unknown} ${ImageVersion:-unknown}"
  echo "system $(uname -s) $(uname -r) $(uname -m)"
  (cd "$ROOT" && rustc --version && cargo --version)
  if [[ "$PLATFORM" == darwin-* ]]; then
    echo "signature ad-hoc (codesign --sign -), not notarized"
  fi
  if [[ "$PLATFORM" == linux-* ]]; then
    newest="$(for file in "${files[@]}"; do glibc_of "$PKG/$file"; done | sort -uV | tail -n 1)"
    echo "glibc minimum: $newest"
    echo
    echo "glibc each program needs, and the shared libraries it loads at start:"
    for file in "${files[@]}"; do
      needed="$(objdump -p "$PKG/$file" | awk '$1 == "NEEDED" { print $2 }' | paste -sd' ' -)"
      echo "  $file: glibc $(glibc_of "$PKG/$file"); $needed"
    done
  fi
  echo
  echo "programs (bytes):"
  for file in "${files[@]}"; do
    echo "  $(wc -c < "$PKG/$file" | tr -d ' ') $file"
  done
} > "$PKG/BUILD-INFO.txt"
chmod 644 "$PKG/LICENSE" "$PKG/BUILD-INFO.txt"
files+=(LICENSE BUILD-INFO.txt)

# No macOS extended attributes or ._ files in the archive.
if command -v xattr > /dev/null; then xattr -cr "$PKG"; fi
rm -f "$OUT/dist/$ARCHIVE"
# In the archive the files belong to root (0:0), not to the user that built
# them: GNU tar, run as root, gives unpacked files the owner the archive names.
if [[ "$(tar --version)" == *GNU* ]]; then owner=(--owner=0 --group=0); else owner=(--uid 0 --gid 0); fi
case "$ARCHIVE" in
  *.zip) (cd "$PKG" && 7z a -tzip -bso0 "$OUT/dist/$ARCHIVE" "${files[@]}") ;;
  *) COPYFILE_DISABLE=1 tar -czf "$OUT/dist/$ARCHIVE" "${owner[@]}" --numeric-owner -C "$PKG" "${files[@]}" ;;
esac
(cd "$OUT/dist" && printf '%s  %s\n' "$(sha256_of "$ARCHIVE")" "$ARCHIVE" > "$ARCHIVE.sha256")
cp "$PKG/BUILD-INFO.txt" "$OUT/dist/BUILD-INFO-$PLATFORM.txt"

cat "$OUT/dist/$ARCHIVE.sha256"
cat "$PKG/BUILD-INFO.txt"
