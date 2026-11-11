#!/usr/bin/env bash
# Tests install.sh against fake releases served from 127.0.0.1.
#
#   bash tests/install-sh/run.sh
#
# No network, no sudo, and nothing outside one temporary folder: every run of
# the script gets its own HOME there, an empty environment, and a PATH with
# only the tools the script may use. The "programs" in the fake releases are
# small shell scripts that write down how they were called.
#
# The script under test is a copy of install.sh with three constants replaced:
# the two URLs (to reach the local server) and the public key (the real secret
# key is not here). Nothing else differs, and `constants` below checks the
# real values. Needs minisign, python3, and curl.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="9.9.9-test.1"
SHIMS=(php node npm npx composer)
PROGRAMS=(marsa marsad marsa-shim marsa-helper marsa-gui)
# What install.sh may run, besides minisign. gzip: GNU tar starts it for -z.
TOOLS=(sh id curl tar gzip uname mktemp awk sed grep tr head mkdir chmod mv ln cp rm rmdir dirname cat)

for tool in minisign python3 curl; do
  command -v "$tool" > /dev/null || { echo "run.sh needs $tool" >&2; exit 1; }
done

T="$(mktemp -d)"
server=""
cleanup() {
  if [ -n "$server" ]; then
    kill "$server" 2> /dev/null || true
    wait "$server" 2> /dev/null || true
  fi
  rm -rf "$T"
}
trap cleanup EXIT

failures=0
pass() { echo "ok: $1"; }
fail() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

# ---------------------------------------------------------------- the tools

# bin/: links to the real tools. bin-minisign/: only minisign, so a PATH can
# leave it out.
mkdir "$T/bin" "$T/bin-minisign"
for tool in "${TOOLS[@]}"; do ln -s "$(command -v "$tool")" "$T/bin/$tool"; done
if command -v sha256sum > /dev/null; then
  ln -s "$(command -v sha256sum)" "$T/bin/sha256sum"
else
  ln -s "$(command -v shasum)" "$T/bin/shasum"
fi
ln -s "$(command -v minisign)" "$T/bin-minisign/minisign"

# answer <dir> <program> <case arms>: a program that prints fixed answers.
answer() {
  mkdir -p "$1"
  cat > "$1/$2" << EOF
#!/bin/sh
case "\${1:-}" in $3 esac
EOF
  chmod 755 "$1/$2"
}
# `uname` as four machines, `sysctl` as a Mac that is not under Rosetta, and
# an `id` that says root.
answer "$T/as-mac" uname '-s) echo Darwin ;; -m) echo arm64 ;;'
answer "$T/as-mac" sysctl '*) echo 0 ;;'
answer "$T/as-linux" uname '-s) echo Linux ;; -m) echo x86_64 ;;'
answer "$T/as-bsd" uname '-s) echo FreeBSD ;; -m) echo amd64 ;;'
answer "$T/as-riscv" uname '-s) echo Linux ;; -m) echo riscv64 ;;'
answer "$T/as-root" id '*) echo 0 ;;'
MAC="$T/as-mac:$T/bin-minisign:$T/bin"
LINUX="$T/as-linux:$T/bin-minisign:$T/bin"
MAC_NO_MINISIGN="$T/as-mac:$T/bin"

sha256_of() { (sha256sum "$1" 2> /dev/null || shasum -a 256 "$1") | cut -d' ' -f1; }

# ----------------------------------------------------------------- the keys

# Two key pairs made now. `good` plays Marsa's key; `other` is someone else's.
for key in good other; do
  minisign -G -W -f -p "$T/$key.pub" -s "$T/$key.key" > /dev/null
done
GOOD_KEY="$(sed -n 2p "$T/good.pub")"

# ------------------------------------------------------------ fake releases

# stub <file> <name> <version>: a program that records its calls in
# <data folder>/calls.log and answers the two commands install.sh runs.
stub() {
  cat > "$1" << EOF
#!/bin/sh
echo "$2-$3 \$*" >> "\$(dirname "\$0")/../calls.log"
case "\$*" in
  --version) echo "$2 $3" ;;
  "daemon stop") echo "Stopped marsad." ;;
esac
EOF
  chmod 755 "$1"
}

# release <case> <platform>: writes www/<case>/download/v<version>/ with the
# archive, SHA256SUMS, and a good signature. Sets DIR and ARCHIVE.
release() {
  local name
  DIR="$T/www/$1/download/v$VERSION"
  ARCHIVE="marsa-$VERSION-$2.tar.gz"
  rm -rf "$T/pkg" "$T/www/$1"
  mkdir -p "$T/pkg" "$DIR"
  for name in "${PROGRAMS[@]}"; do stub "$T/pkg/$name" "$name" "$VERSION"; done
  echo license > "$T/pkg/LICENSE"
  tar -czf "$DIR/$ARCHIVE" -C "$T/pkg" "${PROGRAMS[@]}" LICENSE
  sums
}
# sums: writes SHA256SUMS for $DIR/$ARCHIVE and signs it as this release.
sums() {
  printf '%s  %s\n' "$(sha256_of "$DIR/$ARCHIVE")" "$ARCHIVE" > "$DIR/SHA256SUMS"
  sign good "marsa release v$VERSION"
}
# sign <key> <trusted comment>
sign() {
  rm -f "$DIR/SHA256SUMS.minisig"
  minisign -S -s "$T/$1.key" -m "$DIR/SHA256SUMS" -t "$2" > /dev/null
}

# ------------------------------------------------------- running the script

# script_for <case>: writes the copy of install.sh that downloads from that
# case, and prints its path.
script_for() {
  local copy="$T/install-$1.sh" base="http://127.0.0.1:$PORT/$1" changed
  sed -e "s|^RELEASES_API=.*|RELEASES_API=\"$base/api/releases\"|" \
    -e "s|^DOWNLOAD_URL=.*|DOWNLOAD_URL=\"$base/download\"|" \
    -e "s|^PUBLIC_KEY=.*|PUBLIC_KEY=\"$GOOD_KEY\"|" "$ROOT/install.sh" > "$copy"
  changed="$(diff "$ROOT/install.sh" "$copy" | grep -c '^>' || true)"
  [ "$changed" -eq 3 ] || { echo "the copy of install.sh differs in $changed lines, not 3" >&2; return 1; }
  echo "$copy"
}

# home <case> <path dirs>: a new, empty HOME for the case. Sets HOME_DIR and
# DATA, the data folder the script must choose there.
home() {
  HOME_DIR="$T/home/$1"
  rm -rf "$HOME_DIR"
  mkdir -p "$HOME_DIR"
  case "$2" in
    *as-linux*) DATA="$HOME_DIR/.local/share/marsa" ;;
    *) DATA="$HOME_DIR/Library/Application Support/dev.marsa.Marsa" ;;
  esac
}

# run <case> <path dirs> [NAME=value...]: runs the script in the case's HOME
# with an empty environment. Sets STATUS and OUTPUT (stdout and stderr).
run() {
  local script path="$2"
  script="$(script_for "$1")"
  shift 2
  STATUS=0
  OUTPUT="$(env -i HOME="$HOME_DIR" PATH="$path" SHELL=/bin/zsh "$@" sh "$script" 2>&1)" || STATUS=$?
}

# old_install: an older Marsa in $DATA, as an earlier release left it.
old_install() {
  local name
  mkdir -p "$DATA/bin" "$DATA/shims"
  for name in "${PROGRAMS[@]}"; do stub "$DATA/bin/$name" "$name" 0.0.1; done
  for name in "${SHIMS[@]}"; do ln "$DATA/bin/marsa-shim" "$DATA/shims/$name"; done
}
# snapshot: every file under bin/ and shims/, with its checksum.
snapshot() { (cd "$DATA" && find bin shims -type f -exec cksum {} + | sort -k3); }

# ------------------------------------------------------------- expectations

report() {
  if [ -z "$2" ]; then pass "$1"; else
    fail "$1:$2"
    echo "$OUTPUT" >&2
  fi
}

# installed <name>: the run succeeded and the new programs are in place.
installed() {
  local name problems=""
  [ "$STATUS" -eq 0 ] || problems+=" exit=$STATUS"
  for name in "${PROGRAMS[@]}"; do
    [ "$("$DATA/bin/$name" --version 2> /dev/null)" = "$name $VERSION" ] || problems+=" bin/$name"
  done
  for name in "${SHIMS[@]}"; do
    cmp -s "$DATA/bin/marsa-shim" "$DATA/shims/$name" || problems+=" shims/$name"
  done
  # Only the user may write to bin/: marsad checks that before the helper runs.
  [ -z "$(find "$DATA/bin" -maxdepth 0 \( -perm -020 -o -perm -002 \))" ] || problems+=" bin-is-writable-by-others"
  [ -z "$(find "$DATA" -maxdepth 1 -name '.install.*')" ] || problems+=" leftover-temp"
  [[ "$OUTPUT" == *"Installed: marsa $VERSION"* ]] || problems+=" no-installed-line"
  report "$1" "$problems"
}

# refused <name> <text> [snapshot]: the run failed with that message. With a
# snapshot: bin/ and shims/ are as they were, the old Marsa was not stopped,
# and no downloaded program ran. Without: there is no data folder at all.
refused() {
  local problems=""
  [ "$STATUS" -ne 0 ] || problems+=" exit=0"
  [[ "$OUTPUT" == *"$2"* ]] || problems+=" message"
  if [ -n "${3:-}" ]; then
    [ "$(snapshot)" = "$3" ] || problems+=" files-changed"
    [ ! -e "$DATA/calls.log" ] || problems+=" a-program-ran"
  else
    [ ! -e "$DATA" ] || problems+=" data-folder-left"
  fi
  [ -z "$(find "$HOME_DIR" -name '.install.*')" ] || problems+=" leftover-temp"
  report "$1" "$problems"
}

# refuse <name> <case> <text> [path dirs]: runs the case over an older install,
# which must stay as it was.
refuse() {
  local before
  home "$2" "${4:-$MAC}"
  old_install
  before="$(snapshot)"
  run "$2" "${4:-$MAC}" MARSA_VERSION="$VERSION"
  refused "$1" "$3" "$before"
}

# --------------------------------------------------------------- the server

PORT="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
mkdir "$T/www"
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$T/www" > "$T/server.log" 2>&1 &
server=$!
for _ in $(seq 50); do
  if curl -fsS -o /dev/null "http://127.0.0.1:$PORT/" 2> /dev/null; then break; fi
  sleep 0.2
done

# ================================================================ the cases

# The constants of the real script: Marsa's key, and GitHub over HTTPS.
constants() {
  local key
  key="$(sed -n 's|.*Some("\(RW[A-Za-z0-9+/]\{54\}\)").*|\1|p' "$ROOT/crates/core/src/installer/manifest.rs")"
  [ "${#key}" -eq 56 ] &&
    grep -qx "PUBLIC_KEY=\"$key\"" "$ROOT/install.sh" &&
    grep -qx 'RELEASES_API="https://api.github.com/repos/XJOC/marsa/releases?per_page=100"' "$ROOT/install.sh" &&
    grep -qx 'DOWNLOAD_URL="https://github.com/XJOC/marsa/releases/download"' "$ROOT/install.sh"
}
if constants; then pass "the real script trusts the key in manifest.rs and downloads from GitHub over HTTPS"; else fail "constants"; fi

# --- installs

release fresh-mac darwin-arm64
home fresh-mac "$MAC"
run fresh-mac "$MAC" MARSA_VERSION="$VERSION"
installed "a new install on macOS, signature checked"
[[ "$OUTPUT" == *"The signature of SHA256SUMS is good."* ]] || fail "fresh-mac: no line about the signature"
# shellcheck disable=SC2016  # the script prints $HOME and $PATH as text
path_line='export PATH="$HOME/Library/Application Support/dev.marsa.Marsa/shims:$HOME/Library/Application Support/dev.marsa.Marsa/bin:$PATH"'
[[ "$OUTPUT" == *"$path_line"* ]] || fail "fresh-mac: no PATH line"
if [ -e "$HOME_DIR/.zshrc" ] || [ -e "$HOME_DIR/.zprofile" ]; then fail "fresh-mac: the script wrote a shell file"; fi

# As `curl ... | sh` runs it: the script comes from stdin.
home fresh-mac "$MAC"
STATUS=0
OUTPUT="$(env -i HOME="$HOME_DIR" PATH="$MAC" MARSA_VERSION="$VERSION" sh < "$(script_for fresh-mac)" 2>&1)" || STATUS=$?
installed "the script piped into sh"

release fresh-linux linux-x64
home fresh-linux "$LINUX"
run fresh-linux "$LINUX" MARSA_VERSION="v$VERSION"
installed "a new install on Linux, in ~/.local/share/marsa (MARSA_VERSION with a v)"

release xdg linux-x64
home xdg "$LINUX"
DATA="$HOME_DIR/data/marsa"
run xdg "$LINUX" MARSA_VERSION="$VERSION" XDG_DATA_HOME="$HOME_DIR/data"
installed "Linux with XDG_DATA_HOME"
home xdg "$LINUX"
run xdg "$LINUX" MARSA_VERSION="$VERSION" XDG_DATA_HOME="relative/data"
installed "Linux with an XDG_DATA_HOME that is not a full path: ignored"
# A quote in the folder name: the ready-made command would break, so the
# script must name the two folders instead.
home xdg "$LINUX"
DATA="$HOME_DIR/it's data/marsa"
run xdg "$LINUX" MARSA_VERSION="$VERSION" XDG_DATA_HOME="$HOME_DIR/it's data"
installed "Linux with a quote and a space in XDG_DATA_HOME"
if [[ "$OUTPUT" != *"Add these two folders to the front of your PATH"* ]] || [[ "$OUTPUT" == *"echo '"* ]]; then
  fail "xdg: a ready-made command was printed for a folder with a quote"
fi

# The list also holds releases of other things, and a "tag_name" inside text.
release newest darwin-arm64
mkdir -p "$T/www/newest/api"
cat > "$T/www/newest/api/releases" << EOF
[
  {
    "tag_name": "runtimes-manifest",
    "body": "not a tag: \"tag_name\": \"v7.7.7\""
  },
  {"tag_name":"php-8.4.27-r1","prerelease":true},
  {"tag_name": "v$VERSION", "prerelease": true},
  {"tag_name": "v0.0.1", "prerelease": true}
]
EOF
home newest "$MAC"
run newest "$MAC"
installed "no MARSA_VERSION: the newest v* release of the list"

release upgrade darwin-arm64
home upgrade "$MAC"
old_install
run upgrade "$MAC" MARSA_VERSION="$VERSION"
installed "an upgrade over an older Marsa"
[ "$(head -n 1 "$DATA/calls.log")" = "marsa-0.0.1 daemon stop" ] || fail "upgrade: the old marsa was not asked to stop first"
[[ "$OUTPUT" == *"was stopped for the update"* ]] || fail "upgrade: no line about the stopped Marsa"

release no-minisign darwin-arm64
rm "$DIR/SHA256SUMS.minisig"
home no-minisign "$MAC_NO_MINISIGN"
run no-minisign "$MAC_NO_MINISIGN" MARSA_VERSION="$VERSION"
installed "no minisign program: installs after the SHA-256 check alone"
[ "$(grep -c 'signature was NOT checked' <<< "$OUTPUT")" -eq 1 ] || fail "no-minisign: not exactly one warning line"

release comment-with-more darwin-arm64
sign good "marsa release v$VERSION built 2026-11-11"
home comment-with-more "$MAC"
run comment-with-more "$MAC" MARSA_VERSION="$VERSION"
installed "a trusted comment with more text after the version and a space"

# --- refusals over an older install

release bad-checksum darwin-arm64
echo more >> "$DIR/$ARCHIVE"
refuse "a changed archive (wrong SHA-256)" bad-checksum "the SHA-256 of $ARCHIVE is wrong"
refuse "a changed archive, without minisign" bad-checksum "the SHA-256 of $ARCHIVE is wrong" "$MAC_NO_MINISIGN"

# The realistic attack: the archive and its checksum are both replaced.
release changed-sums darwin-arm64
echo more >> "$DIR/$ARCHIVE"
printf '%s  %s\n' "$(sha256_of "$DIR/$ARCHIVE")" "$ARCHIVE" > "$DIR/SHA256SUMS"
refuse "a changed archive with a matching SHA256SUMS (wrong signature)" changed-sums "the signature of SHA256SUMS is wrong"

release other-key darwin-arm64
sign other "marsa release v$VERSION"
refuse "SHA256SUMS signed by another key" other-key "the signature of SHA256SUMS is wrong"

# A good signature of another release, with its trusted comment edited.
release edited-comment darwin-arm64
sign good "marsa release v0.0.1"
sed "s/^trusted comment: .*/trusted comment: marsa release v$VERSION/" "$DIR/SHA256SUMS.minisig" > "$T/edited"
mv "$T/edited" "$DIR/SHA256SUMS.minisig"
refuse "a trusted comment edited after signing" edited-comment "the signature of SHA256SUMS is wrong"

for comment in "marsa runtimes manifest serial=1" "marsa release v0.0.1" "marsa release v${VERSION}0" "marsa release v$VERSION-rc" "release v$VERSION"; do
  release wrong-comment darwin-arm64
  sign good "$comment"
  refuse "Marsa's key, but signed as '$comment'" wrong-comment "but not as the release v$VERSION"
done

release no-minisig darwin-arm64
rm "$DIR/SHA256SUMS.minisig"
refuse "no SHA256SUMS.minisig, with minisign installed" no-minisig "cannot download SHA256SUMS.minisig"

release not-listed darwin-arm64
printf '%s  %s\n' "$(sha256_of "$DIR/$ARCHIVE")" "marsa-$VERSION-linux-x64.tar.gz" > "$DIR/SHA256SUMS"
sign good "marsa release v$VERSION"
refuse "SHA256SUMS without this archive" not-listed "does not list $ARCHIVE exactly once"

release listed-twice darwin-arm64
cat "$DIR/SHA256SUMS" "$DIR/SHA256SUMS" > "$T/twice"
mv "$T/twice" "$DIR/SHA256SUMS"
sign good "marsa release v$VERSION"
refuse "SHA256SUMS with this archive twice" listed-twice "does not list $ARCHIVE exactly once"

release missing-program darwin-arm64
tar -czf "$DIR/$ARCHIVE" -C "$T/pkg" marsa marsad marsa-shim marsa-gui LICENSE
sums
refuse "an archive without marsa-helper" missing-program "does not hold the program marsa-helper"

release no-release darwin-arm64
rm -r "$DIR"
refuse "a version that has no release" no-release "cannot download SHA256SUMS"

# --- refusals before anything is downloaded: no data folder is left

release early darwin-arm64
home early "$MAC"
# shellcheck disable=SC2016  # the text $(id) itself is the value
for version in "1.0" "1.0.0/../x" "1.0.0;id" "1.0.0 2.0.0" '$(id)' "1.0.0-"; do
  run early "$MAC" MARSA_VERSION="$version"
  refused "MARSA_VERSION='$version'" "It must be a version such as"
done
run early "$T/as-bsd:$T/bin-minisign:$T/bin" MARSA_VERSION="$VERSION"
refused "a system that is not macOS or Linux" "no build for the system 'FreeBSD'"
run early "$T/as-riscv:$T/bin-minisign:$T/bin" MARSA_VERSION="$VERSION"
refused "a processor that is not arm64 or x86_64" "no build for the processor 'riscv64'"
run early "$T/as-root:$MAC" MARSA_VERSION="$VERSION"
refused "run as root" "not as root and not with sudo"
mkdir "$T/bin-no-curl"
for tool in "$T"/bin/*; do ln -s "$(readlink "$tool")" "$T/bin-no-curl/$(basename "$tool")"; done
rm "$T/bin-no-curl/curl"
run early "$T/as-mac:$T/bin-no-curl" MARSA_VERSION="$VERSION"
refused "no curl" "the program 'curl' is needed"
mkdir -p "$T/www/early/api"
echo '[{"tag_name": "php-8.4.27-r1"}, {"tag_name": "v1.0.0\"; id"}]' > "$T/www/early/api/releases"
run early "$MAC"
refused "a list of releases with no usable v* tag" "found no Marsa release on GitHub"

echo
if [ "$failures" -eq 0 ]; then echo "install.sh tests: all passed"; else
  echo "install.sh tests: $failures FAILED" >&2
  exit 1
fi
