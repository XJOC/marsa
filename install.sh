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
#   4. It unpacks the archive, stops a running Marsa, moves the programs into
#      <data folder>/bin, and links the shims in <data folder>/shims.
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

# fetch <url> <file>. HTTPS only, redirects too. Plain HTTP is allowed for
# 127.0.0.1 alone: the script's own tests serve their files from there.
fetch() {
  case "$1" in
    https://*) curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1" ;;
    http://127.0.0.1:*) curl --proto '=http' -fsS -o "$2" "$1" ;;
    *) return 1 ;;
  esac
}

# Sets VERSION, from MARSA_VERSION or from the newest release on GitHub whose
# tag is v<version>. Pre-releases count: GitHub lists the newest first.
find_version() {
  if [ -n "${MARSA_VERSION:-}" ]; then
    VERSION="${MARSA_VERSION#v}"
    valid_version "$VERSION" || fail "MARSA_VERSION is '$MARSA_VERSION'. It must be a version such as 0.1.0-alpha.1."
    return 0
  fi
  fetch "$RELEASES_API" "$TMP/releases.json" ||
    fail "cannot read the list of releases from GitHub. Try again later, or choose a version: MARSA_VERSION=<version>."
  VERSION="$(tr ',' '\n' < "$TMP/releases.json" | sed -n 's/.*"tag_name" *: *"v\([^"]*\)".*/\1/p' | head -n 1)"
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
    say "Warning: the signature was NOT checked, because the minisign program is not installed. Only the SHA-256 was checked. To check the signature yourself, install minisign, download SHA256SUMS and SHA256SUMS.minisig from $BASE, and run: minisign -V -P $PUBLIC_KEY -m SHA256SUMS"
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

# Unpacks the checked archive inside the temporary folder.
unpack() {
  mkdir "$TMP/unpacked"
  tar -xzf "$TMP/$ARCHIVE" -C "$TMP/unpacked" || fail "cannot unpack $ARCHIVE. Nothing was installed."
  for name in $PROGRAMS; do
    if [ ! -f "$TMP/unpacked/$name" ] || [ -L "$TMP/unpacked/$name" ]; then
      fail "$ARCHIVE does not hold the program $name. Nothing was installed."
    fi
  done
}

# Stops the old Marsa, then puts the new programs and the shims in place.
install_programs() {
  BIN="$DATA/bin"
  SHIM_DIR="$DATA/shims"
  STOPPED=""
  if [ -f "$BIN/marsa" ] && [ -x "$BIN/marsa" ]; then
    # The program of the release that is installed now, not a downloaded one.
    if stopped="$("$BIN/marsa" daemon stop 2> /dev/null)"; then
      case "$stopped" in
        *"not running"*) ;;
        *) STOPPED=yes ;;
      esac
    else
      say "Warning: 'marsa daemon stop' failed. If the old Marsa still runs, stop it, then start it again."
    fi
  fi
  mkdir -p "$BIN" "$SHIM_DIR"
  # marsad runs marsa-helper as root only from a folder that nobody else can
  # write to.
  chmod 755 "$BIN" "$SHIM_DIR"
  for name in $PROGRAMS $OPTIONAL_PROGRAMS; do
    if [ -f "$TMP/unpacked/$name" ] && [ ! -L "$TMP/unpacked/$name" ]; then
      chmod 755 "$TMP/unpacked/$name"
      # Same volume, so this is one rename: a program is never half written.
      mv -f "$TMP/unpacked/$name" "$BIN/$name"
    fi
  done
  # One program under five names. A hard link; a copy where links fail.
  for name in $SHIMS; do
    ln -f "$BIN/marsa-shim" "$SHIM_DIR/$name" 2> /dev/null || cp -f "$BIN/marsa-shim" "$SHIM_DIR/$name"
  done
}

# Prints the line that adds Marsa to PATH. The script does not edit shell files.
say_path() {
  say
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
    */bash-darwin) files=".bash_profile" ;;
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

main() {
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
  # New files and folders: the user may write, others may only read.
  umask 022

  find_platform
  find_data_dir

  # Inside the data folder, so the last move is a rename on one volume.
  mkdir -p "$DATA"
  TMP="$(mktemp -d "$DATA/.install.XXXXXX")" || fail "cannot make a temporary folder in $DATA."
  # Also on a failure. `rmdir` removes the data folder only if it is still empty.
  trap 'rm -rf "$TMP"; rmdir "$DATA" 2> /dev/null || true' EXIT
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

  # Our own program, checked above. It also proves that it starts here.
  installed="$("$BIN/marsa" --version 2> /dev/null)" ||
    fail "Marsa is in $BIN, but 'marsa --version' does not start on this system. On Linux, Marsa needs glibc; the release notes say which version."
  say
  say "Installed: $installed"
  if [ -n "$STOPPED" ]; then
    say "Marsa was running and was stopped for the update. Start it again: marsa daemon start"
  fi
  say_path
  if [ "${PLATFORM%%-*}" = linux ]; then
    say
    say "On Linux, the window (marsa-gui) needs a desktop session with Vulkan or OpenGL. The tray icon (marsa-tray), when this release has it, also needs GTK 3, libxdo, and AppIndicator. On Debian and Ubuntu: sudo apt install libgtk-3-0 libxdo3 libayatana-appindicator3-1"
  fi
  say
  say "Next, in a new terminal:"
  say "  marsa install php 8.4"
  if [ "${PLATFORM%%-*}" = darwin ]; then
    say "  marsa setup    (HTTPS and .test names; asks for your password once)"
  fi
  say "  marsa --help"
}

main "$@"
