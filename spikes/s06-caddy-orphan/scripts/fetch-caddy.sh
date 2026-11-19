#!/bin/bash
# Downloads the official Caddy release for macOS arm64 into vendor/ and checks
# its SHA-512 against the value pinned below. The pinned value was copied from
# caddy_2.11.7_checksums.txt in the same release. This catches a corrupt
# download; it does not check Caddy's signature (out of scope for the spike).
# Nothing is installed outside this folder.
set -euo pipefail

VERSION="2.11.7"
ASSET="caddy_${VERSION}_mac_arm64.tar.gz"
EXPECTED_SHA512="68f592df8f0ec058fbdb9b137b64e9a0aa792d28c06e1e3b9ec12d0e7fb371683a54e73ef6d8e6d0dd70dc6b941a5b5d5b73faedd3f89de119ad2eb78bdc668e"
URL="https://github.com/caddyserver/caddy/releases/download/v${VERSION}/${ASSET}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="${ROOT}/vendor"

if [ -x "${VENDOR}/caddy" ] && "${VENDOR}/caddy" version | grep -q "v${VERSION}"; then
  echo "caddy v${VERSION} already in ${VENDOR}"
  exit 0
fi

mkdir -p "${VENDOR}"
TMP="$(mktemp -d "${VENDOR}/dl.XXXXXX")"
trap 'rm -rf "${TMP}"' EXIT

curl --fail --silent --show-error --location --output "${TMP}/${ASSET}" "${URL}"
ACTUAL="$(shasum -a 512 "${TMP}/${ASSET}" | cut -d' ' -f1)"
if [ "${ACTUAL}" != "${EXPECTED_SHA512}" ]; then
  echo "FAIL: checksum mismatch for ${ASSET}" >&2
  echo "  expected ${EXPECTED_SHA512}" >&2
  echo "  actual   ${ACTUAL}" >&2
  exit 1
fi
echo "checksum OK (sha512)"

tar -xzf "${TMP}/${ASSET}" -C "${TMP}" caddy
mv "${TMP}/caddy" "${VENDOR}/caddy"
chmod 755 "${VENDOR}/caddy"

# curl does not set the quarantine attribute, so Gatekeeper should not object.
if xattr "${VENDOR}/caddy" | grep -q quarantine; then
  echo "WARN: quarantine attribute present on ${VENDOR}/caddy"
else
  echo "no quarantine attribute"
fi
"${VENDOR}/caddy" version
