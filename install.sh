#!/bin/sh
# Installs Marsa for the current user on macOS or Linux. No sudo.
#
#   curl -fsSL https://raw.githubusercontent.com/XJOC/marsa/main/install.sh | sh
#
# In order:
#   1. It finds the newest release, or takes MARSA_VERSION=0.1.0-alpha.1.
#   2. It downloads SHA256SUMS and this system's archive into a temporary
#      folder inside Marsa's data folder.
#   3. It checks them. Always: the SHA-256 of the archive against SHA256SUMS.
#      If a `minisign` program is installed: also the signature of SHA256SUMS,
#      with the public key written below. A failed check stops the install
#      before anything is unpacked, and no installed file has changed.
#   4. It unpacks the archive and checks that the new `marsa` starts on this
#      system. Only then does it stop a running Marsa, move the programs into
#      <data folder>/bin, and link the shims in <data folder>/shims.
#
# It never uses sudo, never edits your shell files, and runs no downloaded
# program before the checks pass. `marsa setup` asks for admin rights later.
#
# Known limit (ADR 0013): without `minisign` the signature is not checked, and
# SHA256SUMS comes from the same place as the archive.
#
# The whole script is functions, and the last line starts them: a download
# that stops half way runs nothing.

set -eu

# Where the releases are. No variable replaces these.
RELEASES_API="https://api.github.com/repos/XJOC/marsa/releases?per_page=100"
DOWNLOAD_URL="https://github.com/XJOC/marsa/releases/download"
# The owner's minisign public key: the one built into Marsa
# (MANIFEST_PUBLIC_KEY in crates/core/src/installer/manifest.rs). It is the
# only trusted key, and no variable replaces it.
PUBLIC_KEY="RWTuvZZrimY2ASzeaoWovb0LKYQTrVnNIbXoxoTKGJEZmSeLlyomDKxU"

# The file names are fixed here. Nothing that comes from the network is used
# as a command. One piece of network text reaches a file name and a URL: the
# version of the newest release, and only after `valid_version` accepts it.
PROGRAMS="marsa marsad marsa-shim marsa-helper marsa-gui"
OPTIONAL_PROGRAMS="marsa-tray"
# Text files of the archive, kept next to the programs.
TEXT_FILES="LICENSE BUILD-INFO.txt"
SHIMS="php node npm npx composer"

say() { printf '%s\n' "$*"; }

fail() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" > /dev/null 2>&1 || fail "the program '$1' is needed, but it was not found. Install it, then run this script again."
}

# Sets PLATFORM: darwin-arm64, darwin-x64, linux-x64, or linux-arm64.
find_platform() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "Marsa has no build for the system '$os'. This script installs it on macOS and Linux." ;;
  esac
  case "$arch" in
    arm64 | aarch64) arch=arm64 ;;
    x86_64 | amd64) arch=x64 ;;
    *) fail "Marsa has no build for the processor '$arch'. It supports arm64 and x86_64." ;;
  esac
  # A shell that runs under Rosetta says x86_64 on Apple Silicon.
  if [ "$os-$arch" = darwin-x64 ] && [ "$(sysctl -n sysctl.proc_translated 2> /dev/null || true)" = 1 ]; then
    arch=arm64
  fi
  PLATFORM="$os-$arch"
}

# Sets DATA: the folder the `directories` crate gives Marsa
# (`app_data_dir` in crates/core/src/config.rs).
find_data_dir() {
  case "${HOME:-}" in
    /*) ;;
    *) fail "HOME is not set to a full path, so the folder for Marsa is not known." ;;
  esac
  case "$PLATFORM" in
    darwin-*) DATA="$HOME/Library/Application Support/dev.marsa.Marsa" ;;
    *)
      # XDG_DATA_HOME counts only when it is a full path, as in the crate.
      case "${XDG_DATA_HOME:-}" in
        /*) DATA="$XDG_DATA_HOME/marsa" ;;
        *) DATA="$HOME/.local/share/marsa" ;;
      esac
      ;;
  esac
}

# Succeeds if $1 is a version such as 0.1.0 or 0.1.0-alpha.1 and nothing else.
# The first test refuses every other character, a line break too, so the
# second one sees exactly one line.
valid_version() {
  case "$1" in
    "" | *[!0-9A-Za-z.-]*) return 1 ;;
  esac
  printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'
}

# fetch <url> <file>. HTTPS only, redirects too. A connection that stalls is
# dropped. Plain HTTP is allowed for 127.0.0.1 alone: the script's own tests
# serve their files from there.
fetch() {
  case "$1" in
    https://*)
      curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL --retry 3 \
        --connect-timeout 30 --speed-limit 1024 --speed-time 60 -o "$2" "$1"
      ;;
    http://127.0.0.1:*) curl --proto '=http' -fsS --connect-timeout 5 -o "$2" "$1" ;;
    *) return 1 ;;
  esac
}

# Sets VERSION, from MARSA_VERSION or from the newest release on GitHub whose
# tag is v<version>. Pre-releases count: GitHub lists the newest first. Tags
# of other releases (php-..., runtimes-manifest) and other tags that start
# with v are skipped.
find_version() {
  if [ -n "${MARSA_VERSION:-}" ]; then
    VERSION="${MARSA_VERSION#v}"
    valid_version "$VERSION" || fail "MARSA_VERSION is '$MARSA_VERSION'. It must be a version such as 0.1.0-alpha.1."
    return 0
  fi
  fetch "$RELEASES_API" "$TMP/releases.json" ||
    fail "cannot read the list of releases from GitHub. Try again later, or choose a version: MARSA_VERSION=<version>."
  VERSION="$(tr ',' '\n' < "$TMP/releases.json" |
    sed -n 's/.*"tag_name" *: *"v\([0-9][0-9A-Za-z.-]*\)".*/\1/p' |
    grep -E '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$' | head -n 1)"
  valid_version "$VERSION" ||
    fail "found no Marsa release on GitHub. Choose a version: MARSA_VERSION=<version>."
}

# The SHA-256 of a file, as 64 lowercase hex characters. Empty if the tool
# fails, and an empty value never equals a checksum.
sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum < "$1" | awk '{ print $1 }'
  else
    shasum -a 256 < "$1" | awk '{ print $1 }'
  fi
}

# Checks the minisign signature of SHA256SUMS, if `minisign` is installed.
check_signature() {
  if ! command -v minisign > /dev/null 2>&1; then
    say "Warning: the signature will NOT be checked, because the minisign program is not installed. Only the SHA-256 of the archive will be checked. To check the signature yourself: download SHA256SUMS and SHA256SUMS.minisig from $BASE, run 'minisign -V -P $PUBLIC_KEY -m SHA256SUMS', and make sure it prints the trusted comment 'marsa release v$VERSION'."
    return 0
  fi
  fetch "$BASE/SHA256SUMS.minisig" "$TMP/SHA256SUMS.minisig" ||
    fail "cannot download SHA256SUMS.minisig, so the signature of this release cannot be checked. Nothing was installed."
  # -Q prints the trusted comment, and only after the signature of the file
  # and of the comment are both good.
  comment="$(minisign -Q -V -P "$PUBLIC_KEY" -x "$TMP/SHA256SUMS.minisig" -m "$TMP/SHA256SUMS" 2> /dev/null)" ||
    fail "the signature of SHA256SUMS is wrong: Marsa's key did not sign these files. Nothing was installed."
  # The comment ties the signature to this release. Marsa's key also signs
  # other files (the list of runtimes), and other releases.
  case "$comment" in
    "marsa release v$VERSION" | "marsa release v$VERSION "*) ;;
    *) fail "SHA256SUMS is signed by Marsa's key, but not as the release v$VERSION. Nothing was installed." ;;
  esac
  say "The signature of SHA256SUMS is good."
}

# Checks the archive against its line in SHA256SUMS.
check_archive() {
  want="$(awk -v name="$ARCHIVE" '$2 == name { print $1 }' "$TMP/SHA256SUMS")"
  # Exactly one line, of 64 hex characters. Two lines would hold a line break.
  case "$want" in
    "" | *[!0-9a-f]*) fail "SHA256SUMS does not list $ARCHIVE exactly once. Nothing was installed." ;;
  esac
  [ "${#want}" -eq 64 ] || fail "SHA256SUMS has no SHA-256 for $ARCHIVE. Nothing was installed."
  got="$(sha256_of "$TMP/$ARCHIVE")"
  [ "$got" = "$want" ] ||
    fail "the SHA-256 of $ARCHIVE is wrong (expected $want, got $got). Nothing was installed."
  say "The SHA-256 of $ARCHIVE is good."
}

# Unpacks the checked archive inside the temporary folder, then checks that
# the new marsa starts on this system. Nothing installed has changed yet.
unpack() {
  NEW="$TMP/unpacked"
  mkdir "$NEW"
  tar -xzf "$TMP/$ARCHIVE" -C "$NEW" || fail "cannot unpack $ARCHIVE. Nothing was installed."
  for name in $PROGRAMS; do
    if [ ! -f "$NEW/$name" ] || [ -L "$NEW/$name" ]; then
      fail "$ARCHIVE does not hold the program $name. Nothing was installed."
    fi
  done
  for name in $PROGRAMS $OPTIONAL_PROGRAMS; do
    if [ -f "$NEW/$name" ] && [ ! -L "$NEW/$name" ]; then chmod 755 "$NEW/$name"; fi
  done
  for name in $TEXT_FILES; do
    if [ -f "$NEW/$name" ] && [ ! -L "$NEW/$name" ]; then chmod 644 "$NEW/$name"; fi
  done
  # The first downloaded program to run, after every check. A build for a
  # newer glibc or for another processor stops here, while the installed
  # Marsa still works.
  NEW_VERSION="$("$NEW/marsa" --version 2> /dev/null)" ||
    fail "the new marsa does not start on this system, so nothing was installed. On Linux, Marsa needs a recent glibc: the release notes say which version."
  [ "$NEW_VERSION" = "marsa $VERSION" ] ||
    fail "the archive of v$VERSION holds another version of marsa. Nothing was installed."
}

# Stops the old Marsa, then puts the new programs and the shims in place.
install_programs() {
  BIN="$DATA/bin"
  SHIM_DIR="$DATA/shims"
  mkdir -p "$BIN" "$SHIM_DIR" || fail "cannot make the folders $BIN and $SHIM_DIR. Nothing was installed."
  # marsad runs marsa-helper as root only from a folder that nobody else can
  # write to.
  chmod 755 "$BIN" "$SHIM_DIR" || fail "cannot set the mode of $BIN and $SHIM_DIR. Nothing was installed."

  # From here on the installed Marsa changes. If a step fails, `on_exit`
  # says that the install stopped half way.
  REPLACING=yes
  STOPPED=""
  if [ -f "$BIN/marsa" ] && [ -x "$BIN/marsa" ]; then
    # The program of the release that is installed now, not a downloaded one.
    if "$BIN/marsa" daemon stop > /dev/null 2>&1; then
      STOPPED=yes
    else
      say "Warning: 'marsa daemon stop' failed. If the old Marsa still runs, stop it, then start it again."
    fi
  fi
  for name in $PROGRAMS $OPTIONAL_PROGRAMS $TEXT_FILES; do
    if [ -f "$NEW/$name" ] && [ ! -L "$NEW/$name" ]; then
      # Same volume, so this is one rename: a program is never half written,
      # and a program that still runs keeps its old file.
      mv -f "$NEW/$name" "$BIN/$name"
    fi
  done
  # One program under five names. A hard link; a copy where links fail. The
  # old name goes first: a signed program on macOS must not be overwritten
  # in place.
  for name in $SHIMS; do
    rm -f "$SHIM_DIR/$name"
    ln "$BIN/marsa-shim" "$SHIM_DIR/$name" 2> /dev/null || cp "$BIN/marsa-shim" "$SHIM_DIR/$name"
  done
  REPLACING=""
}

# Prints the line that adds Marsa to PATH. The script does not edit shell files.
say_path() {
  say
  case ":${PATH:-}:" in
    *":$DATA/shims:"*)
      case ":$PATH:" in
        *":$DATA/bin:"*)
          say "Marsa's folders are already in your PATH."
          return 0
          ;;
      esac
      ;;
  esac
  case "$DATA" in
    "$HOME"/*) rest="${DATA#"$HOME"}" shown="\$HOME$rest" ;;
    *) rest="$DATA" shown="$DATA" ;;
  esac
  case "$rest" in
    # These characters would change the meaning of the line below.
    *[\"\'\$\`\\]*)
      say "Add these two folders to the front of your PATH:"
      say "  $DATA/shims"
      say "  $DATA/bin"
      return 0
      ;;
  esac
  line="export PATH=\"$shown/shims:$shown/bin:\$PATH\""
  say "Add Marsa to your PATH. Put this line at the end of your shell's startup file:"
  say
  say "  $line"
  # zsh on macOS reads .zprofile and .zshrc; the line goes last in both, so
  # Marsa's php comes before any other (architecture.md section 10).
  case "${SHELL:-}-${PLATFORM%%-*}" in
    */zsh-darwin) files=".zprofile .zshrc" ;;
    */zsh-linux) files=".zshrc" ;;
    */bash-darwin)
      # A login shell reads only the first of these that exists, so a new
      # .bash_profile would hide an existing .profile.
      files=".bash_profile"
      for file in .bash_profile .bash_login .profile; do
        if [ -f "$HOME/$file" ]; then
          files="$file"
          break
        fi
      done
      ;;
    */bash-linux) files=".bashrc" ;;
    *) return 0 ;;
  esac
  say
  say "These commands do it:"
  say
  for file in $files; do
    say "  echo '$line' >> \"\$HOME/$file\""
  done
  say
  say "Then open a new terminal."
}

# Runs when the script ends, also after a failure or a signal.
on_exit() {
  status=$?
  rm -rf "$TMP"
  # Only if it is still empty: a first install that failed leaves nothing.
  rmdir "$DATA" 2> /dev/null || true
  if [ "$status" -ne 0 ] && [ -n "$REPLACING" ]; then
    printf '%s\n' "install.sh: the install stopped half way. Some programs in $DATA/bin may be new and others old, and Marsa may be stopped. Run this script again to finish it." >&2
  fi
}

main() {
  REPLACING=""
  # Marsa belongs to one user. As root it would land in root's folders, or
  # leave root's files in yours.
  need id
  [ "$(id -u)" != 0 ] || fail "run this script as your own user, not as root and not with sudo."
  need curl
  need tar
  need uname
  need mktemp
  need awk
  need sed
  need grep
  need tr
  need head
  if ! command -v sha256sum > /dev/null 2>&1 && ! command -v shasum > /dev/null 2>&1; then
    fail "a SHA-256 program is needed (sha256sum or shasum), but none was found."
  fi
  # New files and folders are never writable by others, and never looser
  # than the user's own umask.
  umask "$(printf '%03o' $((0$(umask) | 022)))"

  find_platform
  find_data_dir

  # Inside the data folder, so the last move is a rename on one volume.
  mkdir -p "$DATA"
  TMP="$(mktemp -d "$DATA/.install.XXXXXX")" || fail "cannot make a temporary folder in $DATA."
  trap on_exit EXIT
  trap 'exit 1' HUP INT TERM

  find_version
  ARCHIVE="marsa-$VERSION-$PLATFORM.tar.gz"
  BASE="$DOWNLOAD_URL/v$VERSION"
  say "Installing Marsa $VERSION for $PLATFORM into $DATA"

  fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" ||
    fail "cannot download SHA256SUMS of the release v$VERSION from $BASE. Is the version right?"
  check_signature
  fetch "$BASE/$ARCHIVE" "$TMP/$ARCHIVE" || fail "cannot download $ARCHIVE from $BASE."
  check_archive

  unpack
  install_programs

  say
  say "Installed: $NEW_VERSION"
  if [ -n "$STOPPED" ]; then
    say "If Marsa was running, it was stopped for the update. Start it again: marsa daemon start"
  fi
  say_path
  if [ "${PLATFORM%%-*}" = linux ]; then
    say
    say "On Linux, the window (marsa-gui) needs a desktop session with Vulkan or OpenGL, and the menu-bar icon (marsa-tray) needs GTK 3 and AppIndicator. On Debian and Ubuntu: sudo apt install libgtk-3-0 libayatana-appindicator3-1"
  fi
  say
  say "Next:"
  say "  marsa install php 8.4"
  if [ "${PLATFORM%%-*}" = darwin ]; then
    say "  marsa setup    (HTTPS and .test names; asks for your password once)"
  fi
  say "  marsa --help"
}

main "$@"
