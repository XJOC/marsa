#!/bin/sh
# Installs the S-08 spike app on macOS without sudo (task S-08b).
#
#   curl -fsSL https://raw.githubusercontent.com/XJOC/marsa/task/S-08b-install-update/spikes/s08-tauri/install.sh | sh
#
# S08_BASE_URL overrides the download source, so the bad-checksum test points
# at a file:// directory and never touches the published pre-release.
# S08_INSTALL_DIR overrides the destination (default: ~/Applications).

set -eu

APP_NAME="s08-app.app"
ARCHIVE="s08-app-macos.tar.gz"
SUMS="SHA256SUMS"
DEFAULT_BASE_URL="https://github.com/XJOC/marsa/releases/download/spike-s08"

main() {
  base_url="${S08_BASE_URL:-$DEFAULT_BASE_URL}"
  install_dir="${S08_INSTALL_DIR:-$HOME/Applications}"

  if [ ! -d "$install_dir" ]; then
    mkdir -p "$install_dir"
  fi

  # The temp folder sits inside the install folder (same volume) and the trap
  # removes it on every exit path, including a failed checksum.
  tmp=$(mktemp -d "$install_dir/.s08-install.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT INT TERM

  echo "downloading $ARCHIVE and $SUMS from $base_url"
  curl -fsSL "$base_url/$SUMS" -o "$tmp/$SUMS"
  curl -fsSL "$base_url/$ARCHIVE" -o "$tmp/$ARCHIVE"

  # A missing line is a rejection too: verifying nothing would install anything.
  expected=$(awk -v name="$ARCHIVE" '{ n = $2; sub(/^\*/, "", n); if (n == name) { print; exit } }' "$tmp/$SUMS")
  if [ -z "$expected" ]; then
    echo "error: $SUMS has no entry for $ARCHIVE; nothing was installed" >&2
    exit 1
  fi

  printf '%s\n' "$expected" > "$tmp/expected.sha256"
  if ! ( cd "$tmp" && /usr/bin/shasum -a 256 -c expected.sha256 ); then
    echo "error: checksum mismatch for $ARCHIVE; nothing was installed" >&2
    exit 1
  fi

  tar -xzf "$tmp/$ARCHIVE" -C "$tmp"

  # `mv` into an existing bundle moves the app *inside* it, so the fixed path
  # goes first, and only after the checksum passed.
  rm -rf "$install_dir/$APP_NAME"
  mv "$tmp/macos/$APP_NAME" "$install_dir/$APP_NAME"

  echo "installed $install_dir/$APP_NAME"
  echo "open it with: open \"$install_dir/$APP_NAME\""
}

# Called on the last line: a truncated script, as with `curl | sh`, still
# defines `main` completely before anything runs.
main "$@"
