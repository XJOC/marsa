#!/usr/bin/env bash
# Puts the platform archives together for one release: checks each archive
# against its .sha256, adds install.sh, writes SHA256SUMS, and writes the
# release notes.
#
#   VERSION=0.1.0 RELEASE_PLATFORMS="darwin-arm64 darwin-x64" collect.sh <dist folder> <notes file>
#
# The folder holds what package.sh wrote for every platform. RELEASE_PLATFORMS
# names the platforms the release holds (default: all five); the archives of
# the others are dropped. Afterwards the folder holds exactly the files of the
# release: those archives, install.sh, and SHA256SUMS.
set -euo pipefail
# shellcheck source=tools/release/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

USAGE="usage: VERSION=<version> collect.sh <dist folder> <notes file>"
DIST="$(cd "${1:?$USAGE}" && pwd)"
NOTES="${2:?$USAGE}"
case "$NOTES" in /*) ;; *) NOTES="$PWD/$NOTES" ;; esac
: "${VERSION:?VERSION is required}"

# The key that signs SHA256SUMS is the key built into Marsa. Read from the
# code, so the notes can never show another one.
key_file="$ROOT/crates/core/src/installer/manifest.rs"
public_key="$(sed -n 's|.*Some("\(RW[A-Za-z0-9+/]\{54\}\)").*|\1|p' "$key_file")"
[ "${#public_key}" -eq 56 ] || { echo "cannot read one minisign public key from $key_file" >&2; exit 1; }

# The platforms of this release, each one known, in the order of PLATFORMS.
read -r -a wanted <<< "${RELEASE_PLATFORMS:-${PLATFORMS[*]}}"
released=()
for platform in "${PLATFORMS[@]}"; do
  if [[ " ${wanted[*]} " == *" $platform "* ]]; then released+=("$platform"); fi
done
for platform in "${wanted[@]}"; do
  [[ " ${PLATFORMS[*]} " == *" $platform "* ]] || { echo "unknown platform in RELEASE_PLATFORMS: $platform" >&2; exit 1; }
done
[ "${#released[@]}" -gt 0 ] || { echo "RELEASE_PLATFORMS names no platform" >&2; exit 1; }

cd "$DIST"
glibc=""
for platform in "${PLATFORMS[@]}"; do
  set_platform "$platform"
  if [[ " ${released[*]} " != *" $platform "* ]]; then
    rm -f "$ARCHIVE" "$ARCHIVE.sha256" "BUILD-INFO-$platform.txt"
    continue
  fi
  [ -f "$ARCHIVE" ] || { echo "missing $ARCHIVE" >&2; exit 1; }
  [ "$(sha256_of "$ARCHIVE")  $ARCHIVE" = "$(cat "$ARCHIVE.sha256")" ] ||
    { echo "$ARCHIVE does not match $ARCHIVE.sha256" >&2; exit 1; }
  if [[ "$platform" == linux-* ]]; then
    needs="$(sed -n 's/^glibc minimum: //p' "BUILD-INFO-$platform.txt")"
    [ -n "$needs" ] || { echo "BUILD-INFO-$platform.txt has no glibc minimum" >&2; exit 1; }
    glibc="$(printf '%s\n' "$glibc" "$needs" | sort -V | tail -n 1)"
  fi
done
# The install script ships with the release, and the signed list names it too.
cp "$ROOT/install.sh" install.sh
printf '%s  %s\n' "$(sha256_of install.sh)" install.sh > install.sh.sha256
cat ./*.sha256 | sort -k2 > SHA256SUMS
rm ./*.sha256 ./BUILD-INFO-*.txt

# The programs, as the first archive of the release lists them.
set_platform "${released[0]}"
programs=""
tray=""
while read -r name; do
  programs="$programs\`$name\`, "
  if [ "$name" = marsa-tray ]; then
    tray=" \`marsa-tray\` also needs GTK 3 and AppIndicator: \`sudo apt install libgtk-3-0 libayatana-appindicator3-1\` on Debian and Ubuntu."
  fi
done < <(archive_names "$ARCHIVE" | grep '^marsa' | sed 's/\.exe$//')

# One line per system in the release, and one for those left out.
has() { [[ " ${released[*]} " == *" $1"* ]]; }
systems=()
lines=""
if has darwin-; then
  systems+=(macOS)
  lines+="- **macOS** (\`darwin-arm64\` for Apple Silicon, \`darwin-x64\` for Intel): the programs are ad-hoc signed, not notarized by Apple. Download the archive with \`curl\`. A browser marks its downloads as quarantined, and macOS may then refuse to run them."$'\n'
fi
if has linux-; then
  systems+=(Linux)
  lines+="- **Linux** (\`linux-x64\`, \`linux-arm64\`): needs glibc $glibc or newer. \`marsa-gui\` needs a desktop session (X11 or Wayland) with Vulkan or OpenGL.$tray"$'\n'
fi
if has windows-; then
  lines+="- **Windows** (\`windows-x64\`): the programs are not code-signed yet, so SmartScreen may warn."$'\n'
fi
left_out=()
for family in macOS:darwin- Linux:linux- Windows:windows-; do
  if ! has "${family#*:}"; then left_out+=("${family%%:*}"); fi
done
# "A", "A and B", or "A, B, and C".
join_and() {
  case "$#" in
    1) printf '%s' "$1" ;;
    2) printf '%s and %s' "$1" "$2" ;;
    *) printf '%s, %s, and %s' "$1" "$2" "$3" ;;
  esac
}
if [ "${#left_out[@]}" -gt 0 ]; then
  lines+="- Not in this release: $(join_and "${left_out[@]}"). The workflow builds them, but they are not released yet."$'\n'
fi
install=""
if [ "${#systems[@]}" -gt 0 ]; then
  install="## Install on ${systems[0]}${systems[1]:+ or ${systems[1]}}

\`\`\`sh
curl -fsSL https://github.com/XJOC/marsa/releases/download/v$VERSION/install.sh | MARSA_VERSION=$VERSION sh
\`\`\`

The script installs for your user only, without \`sudo\`, and edits no shell file. It checks the SHA-256 of the archive before it unpacks it, and the signature below if \`minisign\` is installed.
"
fi

cat > "$NOTES" << EOF
Marsa $VERSION, a pre-release. Built from commit ${GITHUB_SHA:-unknown} in
${GITHUB_SERVER_URL:-}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}.

$install
## Archives

One archive per system. Each holds ${programs}\`LICENSE\`, and \`BUILD-INFO.txt\` in one flat folder. Keep the programs together: they find each other by folder.

$lines
## Checksums and signature

\`SHA256SUMS\` lists the sha256 of every archive and of \`install.sh\`. \`SHA256SUMS.minisig\` is its [minisign](https://jedisct1.github.io/minisign/) signature. The owner makes it on their own machine, and the secret key is never in CI:

\`\`\`sh
minisign -S -s ~/.minisign/marsa-runtimes.key -m SHA256SUMS -t "marsa release v$VERSION"
\`\`\`

To check it (the trusted comment must start with \`marsa release v$VERSION\`):

\`\`\`sh
minisign -V -P $public_key -m SHA256SUMS
\`\`\`

\`\`\`
$(cat SHA256SUMS)
\`\`\`
EOF

cat SHA256SUMS
