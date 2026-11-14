#!/usr/bin/env bash
# The helper's Linux ops in a container with systemd as PID 1: Fedora
# (systemd 257 or later) and Debian 12 (systemd 252, where the unit adds the
# local route for 127.0.0.1). Task M4-18f; reviews of #114 and #111. Ubuntu
# runs the whole end to end in linux.sh; this adds the other families, a
# real boot, and NetworkManager.
#
# It runs as root inside the container that linux-e2e.yml starts, in two
# phases around a restart of the container, which boots systemd again in a
# new network namespace: marsa0 is gone, and only Marsa's unit makes it
# again.
#
#   container.sh before-boot   install, the forger, trust, NetworkManager
#   container.sh after-boot    the unit ran at boot, then the removal
#
# A stand-in answers on port 18053 as marsad would (spikes/s02-linux).
set -euo pipefail

if [ "${CI:-}" != "true" ]; then
  echo "This script changes system files. It runs only with CI=true (a runner)." >&2
  exit 2
fi

REPO=/repo
HELPER=$REPO/target/release/marsa-helper
STANDIN=$REPO/spikes/s02-linux/dns-standin.py
SITE=laravel-app.test
UNIT=/etc/systemd/system/marsa-dns-link.service
TEST_CA=$REPO/fixtures/helper/test-ca.pem
if [ -d /usr/local/share/ca-certificates ] && [ -x /usr/sbin/update-ca-certificates ]; then
  TRUST_FILE=/usr/local/share/ca-certificates/marsa-local-ca.crt
  BUNDLE=/etc/ssl/certs/ca-certificates.crt
else
  TRUST_FILE=/etc/pki/ca-trust/source/anchors/marsa-local-ca.pem
  # What update-ca-trust extracts; /etc/pki/tls/certs/ca-bundle.crt is not
  # on every Fedora (run 43605058794).
  BUNDLE=/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem
fi
QUERIES=/root/queries.log

step() { printf '\n\n===== %s =====\n' "$*"; }
fail() {
  echo "FAIL: $*" >&2
  resolvectl status marsa0 || true
  systemctl status marsa-dns-link.service --no-pager || true
  exit 1
}
run() {
  printf '$ %s\n' "$*"
  "$@"
}
helper() {
  local request="{\"version\":1,\"ops\":$1}"
  printf '$ marsa-helper apply %s\n' "$request"
  local code=0
  ANSWER=$("$HELPER" apply "$request") || code=$?
  echo "$ANSWER"
  return "$code"
}
statuses() { echo "$ANSWER" | grep -o '"status":"[a-z]*"' | cut -d'"' -f4 | tr '\n' ' ' | sed 's/ $//'; }
address_of() { getent ahostsv4 "$1" | awk 'NR==1 {print $1}' | grep . || echo none; }
wait_for_name() {
  local i
  for i in $(seq 1 20); do
    if [ "$(address_of "$1")" = 127.0.0.1 ]; then
      echo "$1 resolves (try $i)"
      return 0
    fi
    sleep 0.5
  done
  return 1
}
STANDIN_PID=""
standin() {
  if [ -n "$STANDIN_PID" ]; then
    kill "$STANDIN_PID" 2>/dev/null || true
    wait "$STANDIN_PID" 2>/dev/null || true
  fi
  python3 "$STANDIN" 18053 "$QUERIES" ${1:-} &
  STANDIN_PID=$!
  sleep 1
  resolvectl flush-caches
}
in_bundle() { grep -qF "$(sed -n 2p "$1")" "$BUNDLE"; }
trust_ops() {
  python3 -c 'import json, sys; print(json.dumps([{"op": "trust_install", "pem": open(sys.argv[1]).read()}]))' "$1"
}

. /etc/os-release
SYSTEMD=$(systemctl --version | awk 'NR==1 {print $2}')
echo "system: $PRETTY_NAME, systemd $SYSTEMD"

case "${1:-}" in
before-boot)
  step "the helper: .test names through marsa0"
  standin
  helper '[{"op":"resolved_install","port":18053}]' || fail "the helper exited with $?"
  [ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
  [ "$(systemctl is-enabled marsa-dns-link.service)" = enabled ] || fail "the unit is not enabled"
  run resolvectl status marsa0
  run cat "$UNIT"
  if [ "$SYSTEMD" -lt 253 ]; then
    grep -q 'route add local 127.0.0.1/32 dev marsa0' "$UNIT" || fail "no local route in the unit on systemd $SYSTEMD"
    run ip route show table local dev marsa0
  fi
  # The first lookup must be quick: before 253 without the route it took
  # 21 s (spike S-02c).
  start=$(date +%s)
  wait_for_name "$SITE" || fail "$SITE does not resolve"
  took=$(($(date +%s) - start))
  echo "the first lookup took about $took s"
  [ "$took" -le 3 ] || fail "the first lookup took $took s"

  step "a forger on 18053 gets .test names only"
  : >"$QUERIES"
  standin spoof
  for name in example.com github.com; do
    answer=$(address_of "$name")
    echo "$name -> $answer"
    [ "$answer" != 10.9.9.9 ] || fail "the forger answered for $name"
  done
  echo "$SITE -> $(address_of "$SITE") (the forger may answer .test names only)"
  other=$(grep -c ' other ' "$QUERIES" 2>/dev/null || true)
  echo "queries outside .test at port 18053: ${other:-0}"
  [ "${other:-0}" = 0 ] || fail "$other queries outside .test reached port 18053"
  standin

  step "trust in the system store"
  helper "$(trust_ops "$TEST_CA")" || fail "the helper exited with $?"
  [ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
  [ "$(stat -c '%U:%G %a' "$TRUST_FILE")" = "root:root 644" ] || fail "wrong owner or mode on $TRUST_FILE"
  in_bundle "$TEST_CA" || fail "the CA is not in $BUNDLE"
  run openssl verify -CAfile "$BUNDLE" "$TEST_CA"
  helper '[{"op":"trust_remove"}]' || fail "the helper exited with $?"
  [ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
  [ ! -e "$TRUST_FILE" ] || fail "$TRUST_FILE is still there"
  ! in_bundle "$TEST_CA" || fail "the CA is still in $BUNDLE"

  step "NetworkManager starts, reloads, and restarts next to marsa0"
  run systemctl start NetworkManager
  run nmcli general reload
  run systemctl restart NetworkManager
  sleep 2
  run nmcli device status
  run resolvectl status marsa0
  wait_for_name "$SITE" || fail "$SITE does not resolve with NetworkManager"
  echo
  echo "PASS: $ID, before the boot"
  ;;
after-boot)
  step "after the boot: the unit made marsa0 again"
  standin
  [ "$(systemctl is-active marsa-dns-link.service)" = active ] || fail "the unit did not run at boot"
  run ip -brief addr show marsa0
  run resolvectl status marsa0
  wait_for_name "$SITE" || fail "$SITE does not resolve after the boot"
  helper '[{"op":"resolved_install","port":18053}]' || fail "the helper exited with $?"
  [ "$(statuses)" = "unchanged" ] || fail "expected unchanged, got: $(statuses)"

  step "removal"
  helper '[{"op":"resolved_remove"}]' || fail "the helper exited with $?"
  [ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
  [ ! -e "$UNIT" ] || fail "$UNIT is still there"
  ! ip link show marsa0 >/dev/null 2>&1 || fail "marsa0 is still there"
  if systemctl --failed --no-legend | grep -q marsa; then
    fail "a failed unit of Marsa's is still listed"
  fi
  resolvectl flush-caches
  if resolvectl query "$SITE" >/dev/null 2>&1; then
    fail "$SITE still resolves after the removal"
  fi
  echo
  echo "PASS: $ID, after the boot"
  ;;
*)
  echo "usage: container.sh before-boot|after-boot" >&2
  exit 2
  ;;
esac
