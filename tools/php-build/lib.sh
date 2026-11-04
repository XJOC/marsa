#!/usr/bin/env bash
# Helpers shared by the PHP build scripts. Source it; do not run it.

# The repository root and the shared extension list.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
EXTENSIONS="$ROOT/recipes/php-extensions.txt"
PINNED="$ROOT/tools/php-build/downloads.sha256"

# One sort order everywhere, so `comm` and `sort` agree.
export LC_ALL=C

# sha256 of a file, as 64 lowercase hex characters.
sha256_of() { (sha256sum "$1" 2> /dev/null || shasum -a 256 "$1") | cut -d' ' -f1; }

# Stops unless the file's sha256 is the one listed for its name in downloads.sha256.
check_pinned() {
  local file="$1" name want got
  name="$(basename "$file")"
  want="$(awk -v n="$name" '$1 !~ /^#/ && $2 == n { print $1 }' "$PINNED")"
  got="$(sha256_of "$file")"
  if [ -z "$want" ]; then
    echo "$name is not listed in $PINNED" >&2
    exit 1
  fi
  if [ "$want" != "$got" ]; then
    echo "$name: sha256 is $got, but $PINNED says $want" >&2
    exit 1
  fi
  echo "sha256 ok: $name $got"
}

# The spc names of one kind (static or shared), joined with commas.
spc_names() { awk -v k="$1" '$1 == k { print $2 }' "$EXTENSIONS" | paste -sd, -; }

# What `php -m` must list: every builtin and static module, lowercase, sorted.
expected_modules() {
  awk '$1 == "builtin" || $1 == "static" { $1 = ""; $2 = ""; sub(/^ +/, ""); print }' "$EXTENSIONS" |
    tr 'A-Z' 'a-z' | sort -u
}

# "module|spc name" for every shared extension.
shared_modules() { awk '$1 == "shared" { m = $3; for (i = 4; i <= NF; i++) m = m " " $i; print m "|" $2 }' "$EXTENSIONS"; }

# Downloads a URL to a file, with retries.
fetch() { curl -fsSL --retry 3 --retry-delay 5 -o "$2" "$1"; }
