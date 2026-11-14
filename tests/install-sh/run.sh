#!/usr/bin/env bash
# Tests install.sh against fake releases served from 127.0.0.1.
#
#   bash tests/install-sh/run.sh
#
# No network, no sudo, and nothing outside one temporary folder: every run of
# the script gets its own HOME there, an empty environment, and a PATH with
# only the tools the script may use.
#
# Every "program" in a fake release is a small shell script that writes each
# call into one log outside the data folder, and the `tar` the script gets
# does the same. So a refused install shows how far it went: whether it
# unpacked anything, and which program, old or new, it ran.
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
# What install.sh may run, besides minisign and a SHA-256 tool. gzip: GNU tar
# starts it for -z.
TOOLS=(sh id curl tar gzip uname mktemp awk sed grep tr head mkdir chmod mv ln cp rm rmdir)
if command -v sha256sum > /dev/null; then HASH_TOOL=sha256sum; else HASH_TOOL=shasum; fi

for tool in minisign python3 curl; do
  command -v "$tool" > /dev/null || { echo "run.sh needs $tool" >&2; exit 1; }
done

T="$(mktemp -d)"
server=""
# Keeps the exit status: otherwise a syntax error in this file could end
# with the status of `rm` (0), and CI would read it as a pass.
cleanup() {
  local code=$?
  if [ -n "$server" ]; then
    kill "$server" 2> /dev/null || true
    wait "$server" 2> /dev/null || true
  fi
  rm -rf "$T"
  exit "$code"
}
trap cleanup EXIT

failures=0
pass() { echo "ok: $1"; }
fail() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

# The log of every call to a fake program or to `tar`, one line each.
CALLS="$T/calls.log"
: > "$CALLS"

# ---------------------------------------------------------------- the tools

# make_tools <dir> [tool to leave out...]: a folder with links to the tools
# install.sh may run. Its `tar` logs each call, then runs the real one.
make_tools() {
  local dir="$1" tool real
  shift
  mkdir "$dir"
  for tool in "${TOOLS[@]}" "$HASH_TOOL"; do
    case " $* " in *" $tool "*) continue ;; esac
    real="$(command -v "$tool")"
    if [ "$tool" = tar ]; then
      # shellcheck disable=SC2016  # $* and $@ belong to the wrapper
      printf '#!/bin/sh\necho "tar $*" >> "%s"\nexec "%s" "$@"\n' "$CALLS" "$real" > "$dir/tar"
      chmod 755 "$dir/tar"
    else
      ln -s "$real" "$dir/$tool"
    fi
  done
}
make_tools "$T/bin"
make_tools "$T/bin-no-curl" curl
# minisign alone, so a PATH can leave it out.
mkdir "$T/bin-minisign"
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
# `uname` as six machines, `sysctl` as a Mac with and without Rosetta, and an
# `id` that says root.
answer "$T/as-mac" uname '-s) echo Darwin ;; -m) echo arm64 ;;'
answer "$T/as-mac" sysctl '*) echo 0 ;;'
answer "$T/as-rosetta" uname '-s) echo Darwin ;; -m) echo x86_64 ;;'
answer "$T/as-rosetta" sysctl '*) echo 1 ;;'
answer "$T/as-linux" uname '-s) echo Linux ;; -m) echo x86_64 ;;'
answer "$T/as-linux-arm" uname '-s) echo Linux ;; -m) echo aarch64 ;;'
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

# stub <file> <name> <version> [exit code of --version] [exit code of
# daemon stop]: a program that logs each call as "<name>-<version> <args>"
# and answers the two commands install.sh runs.
stub() {
  cat > "$1" << EOF
#!/bin/sh
echo "$2-$3 \$*" >> "$CALLS"
case "\$*" in
  --version) echo "$2 $3"; exit ${4:-0} ;;
  "daemon stop") echo "Stopped marsad."; exit ${5:-0} ;;
esac
EOF
  chmod 755 "$1"
}

# release <case> <platform>: writes www/<case>/download/v<version>/ with the
# archive, SHA256SUMS, and a good signature. Sets DIR and ARCHIVE. The
# unpacked files stay in $T/pkg, for the cases that change them.
release() {
  local name
  DIR="$T/www/$1/download/v$VERSION"
  ARCHIVE="marsa-$VERSION-$2.tar.gz"
  rm -rf "$T/pkg" "$T/www/$1"
  mkdir -p "$T/pkg" "$DIR"
  for name in "${PROGRAMS[@]}"; do stub "$T/pkg/$name" "$name" "$VERSION"; done
  echo license > "$T/pkg/LICENSE"
  echo "built for a test" > "$T/pkg/BUILD-INFO.txt"
  pack
}
# pack: archives $T/pkg as $DIR/$ARCHIVE, then writes and signs SHA256SUMS.
pack() {
  local files=() file
  for file in "$T/pkg"/*; do files+=("$(basename "$file")"); done
  rm -f "$DIR/$ARCHIVE"
  tar -czf "$DIR/$ARCHIVE" -C "$T/pkg" "${files[@]}"
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

# home <name> <path dirs>: a new, empty HOME. Sets HOME_DIR and DATA, the data
# folder the script must choose there.
home() {
  HOME_DIR="$T/home/$1"
  rm -rf "$HOME_DIR"
  mkdir -p "$HOME_DIR"
  case "$2" in
    *as-linux*) DATA="$HOME_DIR/.local/share/marsa" ;;
    *) DATA="$HOME_DIR/Library/Application Support/dev.marsa.Marsa" ;;
  esac
}

# run <case> <path dirs> [NAME=value...]: runs the script for that case's
# release in HOME_DIR, with an empty environment and the umask RUN_UMASK
# (022 if unset). Sets STATUS, OUTPUT (stdout and stderr), and CALLED (the
# calls the run logged).
run() {
  local script path="$2"
  script="$(script_for "$1")"
  shift 2
  : > "$CALLS"
  STATUS=0
  OUTPUT="$(umask "${RUN_UMASK:-022}" && env -i HOME="$HOME_DIR" PATH="$path" SHELL=/bin/zsh "$@" sh "$script" 2>&1)" || STATUS=$?
  CALLED="$(cat "$CALLS")"
}

# old_install [exit code of daemon stop]: an older Marsa in $DATA, as an
# earlier release left it.
old_install() {
  local name
  mkdir -p "$DATA/bin" "$DATA/shims"
  for name in "${PROGRAMS[@]}"; do stub "$DATA/bin/$name" "$name" 0.0.1 0 "${1:-0}"; done
  for name in "${SHIMS[@]}"; do ln "$DATA/bin/marsa-shim" "$DATA/shims/$name"; done
}
# snapshot: every file under bin/ and shims/, with its checksum.
snapshot() { (cd "$DATA" && find bin shims -type f -exec cksum {} + | sort -k3); }

# line_of <text>: the number of the first line of CALLED that is exactly that.
line_of() { grep -nxF -- "$1" <<< "$CALLED" | head -n 1 | cut -d: -f1 || true; }

# ------------------------------------------------------------- expectations

report() {
  if [ -z "$2" ]; then pass "$1"; else
    fail "$1:$2"
    echo "$OUTPUT" >&2
  fi
}

# installed <name>: the run succeeded and the new Marsa is in place.
installed() {
  local name problems=""
  [ "$STATUS" -eq 0 ] || problems+=" exit=$STATUS"
  for name in "${PROGRAMS[@]}"; do
    [ "$("$DATA/bin/$name" --version 2> /dev/null)" = "$name $VERSION" ] || problems+=" bin/$name"
  done
  for name in "${SHIMS[@]}"; do
    cmp -s "$DATA/bin/marsa-shim" "$DATA/shims/$name" || problems+=" shims/$name"
  done
  for name in LICENSE BUILD-INFO.txt; do
    [ -s "$DATA/bin/$name" ] || problems+=" bin/$name"
  done
  # Only the user may write to bin/ and to marsa-helper: marsad checks both
  # before it runs the helper as root.
  [ -z "$(find "$DATA/bin" "$DATA/bin/marsa-helper" "$DATA/shims" -maxdepth 0 \( -perm -020 -o -perm -002 \))" ] ||
    problems+=" writable-by-others"
  # The download and the unpacking happen in a temporary folder inside the
  # data folder, so the last move is a rename on one volume.
  [[ "$CALLED" == *"tar -xzf $DATA/.install."* ]] || problems+=" temp-not-in-data-folder"
  [ -z "$(find "$DATA" -maxdepth 1 -name '.install.*')" ] || problems+=" leftover-temp"
  [[ "$OUTPUT" == *"Installed: marsa $VERSION"* ]] || problems+=" no-installed-line"
  [ -n "$(line_of "marsa-$VERSION --version")" ] || problems+=" new-marsa-not-tried"
  report "$1" "$problems"
}

# went_too_far <stage>: the calls in CALLED that a run refused at that stage
# must not have made.
#   checks  refused by a check of the downloads: nothing unpacked, nothing run
#   unpack  refused after unpacking: nothing run
#   start   refused when the new marsa did not start: only `marsa --version`
went_too_far() {
  case "$1" in
    checks) printf '%s' "$CALLED" ;;
    unpack) grep -v '^tar ' <<< "$CALLED" || true ;;
    start) grep -v -e '^tar ' -e '^marsa-[^ ]* --version$' <<< "$CALLED" || true ;;
  esac
}

# refused <name> <text> [snapshot]: the run failed with that message and went
# no further than the stage REACHED (default: checks). With a snapshot: bin/
# and shims/ are as they were. Without: no data folder is left.
refused() {
  local problems="" beyond
  [ "$STATUS" -ne 0 ] || problems+=" exit=0"
  [[ "$OUTPUT" == *"$2"* ]] || problems+=" message"
  if [ -n "${3:-}" ]; then
    [ "$(snapshot)" = "$3" ] || problems+=" files-changed"
  else
    [ ! -e "$DATA" ] || problems+=" data-folder-left"
  fi
  [ -z "$(find "$HOME_DIR" -name '.install.*')" ] || problems+=" leftover-temp"
  beyond="$(went_too_far "${REACHED:-checks}")"
  [ -z "$beyond" ] || problems+=" went-too-far:[${beyond//$'\n'/; }]"
  report "$1" "$problems"
}

# refuse <name> <case> <text> [path dirs]: runs the case over an older
# install, which must stay as it was. Its `daemon stop` must not run either.
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
# zsh: .zshrc only. Every interactive zsh reads it; a terminal that is not a
# login shell never reads .zprofile.
[[ "$OUTPUT" == *"echo '$path_line' >> \"\$HOME/.zshrc\""* ]] || fail "fresh-mac: no command for .zshrc"
[[ "$OUTPUT" != *".zprofile"* ]] || fail "fresh-mac: .zprofile was named"
if [ -e "$HOME_DIR/.zshrc" ] || [ -e "$HOME_DIR/.zprofile" ]; then fail "fresh-mac: the script wrote a shell file"; fi

# As `curl ... | sh` runs it: the script comes from a pipe.
# pipe <script text command...>: runs the script it prints through a pipe.
pipe() {
  : > "$CALLS"
  STATUS=0
  OUTPUT="$("$@" | env -i HOME="$HOME_DIR" PATH="$MAC" MARSA_VERSION="$VERSION" sh 2>&1)" || STATUS=$?
  CALLED="$(cat "$CALLS")"
}
script="$(script_for fresh-mac)"
home fresh-mac "$MAC"
pipe cat "$script"
installed "the script piped into sh"
# A download cut off anywhere runs nothing: the last line starts the script.
size="$(wc -c < "$script" | tr -d ' ')"
for cut in "$((size / 3))" "$((size / 2))" "$((size - 20))"; do
  home cut "$MAC"
  pipe head -c "$cut" "$script"
  if [ -e "$DATA" ] || [ -n "$CALLED" ]; then fail "a download cut at byte $cut of $size ran something"; fi
done
home cut "$MAC"
# shellcheck disable=SC2016  # $d is sed's "delete the last line", not a variable
pipe sed '$d' "$script"
if [ -e "$DATA" ] || [ -n "$CALLED" ]; then fail "a download without its last line ran something"; fi
pass "a cut-off download of the script runs nothing"

# The user's PATH already has both folders: no advice to add them again.
home path-set "$MAC"
run fresh-mac "$DATA/shims:$DATA/bin:$MAC" MARSA_VERSION="$VERSION"
installed "an install when PATH already has Marsa's folders"
if [[ "$OUTPUT" != *"already in your PATH"* ]] || [[ "$OUTPUT" == *"export PATH"* ]]; then
  fail "path-set: the PATH advice was printed again"
fi

# The other shells get the right startup file, or no command at all.
problems=""
home bash-mac "$MAC"
run fresh-mac "$MAC" MARSA_VERSION="$VERSION" SHELL=/bin/bash
[[ "$OUTPUT" == *">> \"\$HOME/.bash_profile\""* ]] || problems+=" bash-without-files"
[ ! -e "$HOME_DIR/.bash_profile" ] || problems+=" bash-without-files-wrote-a-file"
home bash-mac-profile "$MAC"
touch "$HOME_DIR/.profile"
run fresh-mac "$MAC" MARSA_VERSION="$VERSION" SHELL=/bin/bash
if [[ "$OUTPUT" != *">> \"\$HOME/.profile\""* ]] || [[ "$OUTPUT" == *".bash_profile"* ]]; then
  problems+=" bash-with-profile"
fi
if [ -s "$HOME_DIR/.profile" ] || [ -e "$HOME_DIR/.bash_profile" ]; then problems+=" bash-with-profile-wrote-a-file"; fi
home fish-mac "$MAC"
run fresh-mac "$MAC" MARSA_VERSION="$VERSION" SHELL=/usr/local/bin/fish
if [[ "$OUTPUT" != *"Add Marsa to your PATH"* ]] || [[ "$OUTPUT" == *"These commands do it"* ]]; then
  problems+=" fish"
fi
report "the PATH advice for bash and other shells on macOS" "$problems"

release fresh-linux linux-x64
home fresh-linux "$LINUX"
run fresh-linux "$LINUX" MARSA_VERSION="v$VERSION"
installed "a new install on Linux, in ~/.local/share/marsa (MARSA_VERSION with a v)"
[[ "$OUTPUT" == *">> \"\$HOME/.zshrc\""* ]] || fail "fresh-linux: no command for .zshrc"
home bash-linux "$LINUX"
run fresh-linux "$LINUX" MARSA_VERSION="$VERSION" SHELL=/bin/bash
[[ "$OUTPUT" == *">> \"\$HOME/.bashrc\""* ]] || fail "bash on Linux: not .bashrc"

release aarch64 linux-arm64
home aarch64 "$T/as-linux-arm"
run aarch64 "$T/as-linux-arm:$T/bin-minisign:$T/bin" MARSA_VERSION="$VERSION"
installed "Linux on aarch64: the linux-arm64 archive"

release rosetta darwin-arm64
home rosetta "$T/as-rosetta"
run rosetta "$T/as-rosetta:$T/bin-minisign:$T/bin" MARSA_VERSION="$VERSION"
installed "a shell under Rosetta on Apple Silicon: the darwin-arm64 archive"

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

# A loose umask must not leave Marsa's folders writable by others; a strict
# one must not be loosened.
release umask linux-x64
home umask-000 "$LINUX"
RUN_UMASK=000 run umask "$LINUX" MARSA_VERSION="$VERSION"
installed "umask 000"
[ -z "$(find "$DATA" "$DATA/shims" -maxdepth 0 \( -perm -020 -o -perm -002 \))" ] ||
  fail "umask 000: the data folder or shims/ is writable by others"
home umask-077 "$LINUX"
RUN_UMASK=077 run umask "$LINUX" MARSA_VERSION="$VERSION"
installed "umask 077"
[ -z "$(find "$HOME_DIR/.local" "$DATA" -maxdepth 0 -perm -004)" ] ||
  fail "umask 077: the script made folders that others can read"

# The list also holds releases of other things, a newer tag that starts with
# v but is not a version, and a "tag_name" inside text.
release newest darwin-arm64
mkdir -p "$T/www/newest/api"
cat > "$T/www/newest/api/releases" << EOF
[
  {
    "tag_name": "runtimes-manifest",
    "body": "not a tag: \"tag_name\": \"v7.7.7\""
  },
  {"tag_name": "vendor-tools", "prerelease": true},
  {"tag_name":"php-8.4.27-r1","prerelease":true},
  {"tag_name": "v$VERSION", "prerelease": true},
  {"tag_name": "v0.0.1", "prerelease": true}
]
EOF
home newest "$MAC"
run newest "$MAC"
installed "no MARSA_VERSION: the newest v<version> release of the list"

release upgrade darwin-arm64
home upgrade "$MAC"
old_install
run upgrade "$MAC" MARSA_VERSION="$VERSION"
installed "an upgrade over an older Marsa"
try="$(line_of "marsa-$VERSION --version")"
stop="$(line_of "marsa-0.0.1 daemon stop")"
if [ -z "$stop" ] || [ -z "$try" ] || [ "$try" -gt "$stop" ]; then
  fail "upgrade: the old marsa must be stopped, and only after the new one was tried"
fi
[[ "$OUTPUT" == *"stopped for the update"* ]] || fail "upgrade: no line about the stopped Marsa"

# An older bin/ and shims/ that the group may write to (umask 002): fixed.
home loose-folders "$MAC"
old_install
chmod 775 "$DATA/bin" "$DATA/shims"
run upgrade "$MAC" MARSA_VERSION="$VERSION"
installed "an upgrade over a bin/ and shims/ that the group may write to"

home stop-fails "$MAC"
old_install 1
run upgrade "$MAC" MARSA_VERSION="$VERSION"
installed "an upgrade when 'marsa daemon stop' fails: a warning, then the install"
[[ "$OUTPUT" == *"'marsa daemon stop' failed"* ]] || fail "stop-fails: no warning"

release no-minisign darwin-arm64
rm "$DIR/SHA256SUMS.minisig"
home no-minisign "$MAC_NO_MINISIGN"
run no-minisign "$MAC_NO_MINISIGN" MARSA_VERSION="$VERSION"
installed "no minisign program: installs after the SHA-256 check alone"
[ "$(grep -c 'signature will NOT be checked' <<< "$OUTPUT")" -eq 1 ] || fail "no-minisign: not exactly one warning line"

release comment-with-more darwin-arm64
sign good "marsa release v$VERSION built 2026-11-11"
home comment-with-more "$MAC"
run comment-with-more "$MAC" MARSA_VERSION="$VERSION"
installed "a trusted comment with more text after the version and a space"

# The other SHA-256 tool than the one the tests above used.
if [ "$HASH_TOOL" = sha256sum ] && command -v shasum > /dev/null; then
  make_tools "$T/bin-shasum" sha256sum
  ln -s "$(command -v shasum)" "$T/bin-shasum/shasum"
  home shasum "$MAC"
  run fresh-mac "$T/as-mac:$T/bin-minisign:$T/bin-shasum" MARSA_VERSION="$VERSION"
  installed "shasum instead of sha256sum"
else
  echo "note: only $HASH_TOOL is here, so the other SHA-256 branch is not tested on this machine"
fi

# --- refusals over an older install, which must stay as it was

release bad-checksum darwin-arm64
echo more >> "$DIR/$ARCHIVE"
refuse "a changed archive (wrong SHA-256)" bad-checksum "the SHA-256 of $ARCHIVE is wrong"
refuse "a changed archive, without minisign" bad-checksum "the SHA-256 of $ARCHIVE is wrong" "$MAC_NO_MINISIGN"
if [ -d "$T/bin-shasum" ]; then
  refuse "a changed archive, with shasum" bad-checksum "the SHA-256 of $ARCHIVE is wrong" "$T/as-mac:$T/bin-shasum"
fi

# The realistic attack: the archive and its checksum are both replaced.
release changed-sums darwin-arm64
echo more >> "$DIR/$ARCHIVE"
printf '%s  %s\n' "$(sha256_of "$DIR/$ARCHIVE")" "$ARCHIVE" > "$DIR/SHA256SUMS"
refuse "a changed archive with a matching SHA256SUMS (wrong signature)" changed-sums "the signature of SHA256SUMS does not verify with Marsa's key"

release other-key darwin-arm64
sign other "marsa release v$VERSION"
refuse "SHA256SUMS signed by another key" other-key "the signature of SHA256SUMS does not verify with Marsa's key"

# A good signature of another release, with its trusted comment edited.
release edited-comment darwin-arm64
sign good "marsa release v0.0.1"
sed "s/^trusted comment: .*/trusted comment: marsa release v$VERSION/" "$DIR/SHA256SUMS.minisig" > "$T/edited"
mv "$T/edited" "$DIR/SHA256SUMS.minisig"
refuse "a trusted comment edited after signing" edited-comment "the signature of SHA256SUMS does not verify with Marsa's key"

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

release no-release darwin-arm64
rm -r "$DIR"
refuse "a version that has no release" no-release "cannot download SHA256SUMS"

# Refused after unpacking: still nothing run, nothing installed.
release missing-program darwin-arm64
rm "$T/pkg/marsa-helper"
pack
REACHED=unpack refuse "an archive without marsa-helper" missing-program "does not hold the program marsa-helper"

release symlink-program darwin-arm64
rm "$T/pkg/marsa-helper"
ln -s marsa "$T/pkg/marsa-helper"
pack
REACHED=unpack refuse "an archive whose marsa-helper is a symlink" symlink-program "does not hold the program marsa-helper"

# Refused when the new marsa was tried: the old Marsa still runs and works.
release cannot-start darwin-arm64
stub "$T/pkg/marsa" marsa "$VERSION" 1
pack
REACHED=start refuse "a new marsa that does not start here (a newer glibc)" cannot-start "does not start on this system"

release other-version darwin-arm64
stub "$T/pkg/marsa" marsa 1.2.3
pack
REACHED=start refuse "an archive that holds another version of marsa" other-version "holds another version of marsa"

# A folder where a program goes: refused before the old Marsa is stopped.
release folder-in-bin darwin-arm64
home folder-in-bin "$MAC"
old_install
rm "$DATA/bin/marsa-gui"
mkdir "$DATA/bin/marsa-gui"
before="$(snapshot)"
run folder-in-bin "$MAC" MARSA_VERSION="$VERSION"
REACHED=start refused "a folder named like a program in bin/" "is a folder, so the program cannot go there" "$before"

# A step after the old Marsa was stopped fails: the script must say so.
release half-way darwin-arm64
home half-way "$MAC"
old_install
rm "$DATA/shims/php"
mkdir -p "$DATA/shims/php/in-the-way"
run half-way "$MAC" MARSA_VERSION="$VERSION"
if [ "$STATUS" -ne 0 ] && [[ "$OUTPUT" == *"the install stopped half way"* ]] && [ -z "$(find "$DATA" -maxdepth 1 -name '.install.*')" ]; then
  pass "a failure after the old Marsa was stopped says that the install stopped half way"
else
  fail "half-way: exit=$STATUS"
  echo "$OUTPUT" >&2
fi

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
run early "$T/as-mac:$T/bin-no-curl" MARSA_VERSION="$VERSION"
refused "no curl" "the program 'curl' is needed"
mkdir -p "$T/www/early/api"
echo '[{"tag_name": "php-8.4.27-r1"}, {"tag_name": "v1.0.0\"; id"}, {"tag_name": "vendor"}]' > "$T/www/early/api/releases"
run early "$MAC"
refused "a list of releases with no usable v<version> tag" "found no Marsa release on GitHub"

echo
if [ "$failures" -eq 0 ]; then echo "install.sh tests: all passed"; else
  echo "install.sh tests: $failures FAILED" >&2
  exit 1
fi
