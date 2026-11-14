#!/usr/bin/env bash
# End to end on Linux: from the built programs to an open Laravel page.
# Task M4-18f, the Linux side of the Phase 1 gate.
#
# It downloads runtimes, creates a Laravel project, and changes the system
# with sudo (a systemd unit, systemd-resolved, and a sysctl). Run it only on a machine
# that is thrown away afterwards: a GitHub runner. It refuses to start
# anywhere else. It is not part of `cargo nextest`.
#
# Before it: `cargo build --release -p marsa -p marsa-helper`, then
# `cargo build --release -p marsa-shim` (the shim is always built alone).
#
# Phase 1 (now): the helper is run with sudo. Phase 2 replaces that step
# with `marsa setup` through pkexec, once setup runs on Linux (task M4-18e).
set -euo pipefail

if [ "${CI:-}" != "true" ]; then
  echo "This script changes system files with sudo. It runs only with CI=true (a runner)." >&2
  exit 2
fi

REPO=$(cd "$(dirname "$0")/../.." && pwd)
DATA=$HOME/.local/share/marsa
BIN=$DATA/bin
SHIMS=$DATA/shims
SITE=laravel-app.test
CODE=$HOME/Code
UNIT=/etc/systemd/system/marsa-dns-link.service
# The global drop-in of earlier builds (task M4-18a): the helper removes it.
DROPIN=/etc/systemd/resolved.conf.d/marsa.conf
SYSCTL_FILE=/etc/sysctl.d/60-marsa.conf
PORT_START=/proc/sys/net/ipv4/ip_unprivileged_port_start
CA=$DATA/caddy/data/caddy/pki/authorities/marsa/root.crt
# Where the helper puts a CA on Debian and Ubuntu (task M4-18c).
TRUST_FILE=/usr/local/share/ca-certificates/marsa-local-ca.crt
BUNDLE=/etc/ssl/certs/ca-certificates.crt
TEST_CA=$REPO/fixtures/helper/test-ca.pem

step() { printf '\n\n===== %s =====\n' "$*"; }
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
# Prints the command, then runs it.
run() {
  printf '$ %s\n' "$*"
  "$@"
}

# What helps to understand a failure. Never the daemon file: it has the token.
diagnose() {
  step "diagnosis after a failure"
  set +e
  "$BIN/marsa" services
  "$BIN/marsa" sites
  "$BIN/marsa" logs caddy | tail -n 40
  ls -la "$DATA" "$DATA/logs"
  tail -n 40 "$DATA"/logs/*.log
  resolvectl status | head -n 40
  systemctl status marsa-dns-link.service --no-pager
  ip -brief addr show marsa0
  networkctl list --no-pager
  echo "--- marsa0, every 0.25 s while the helper ran (newest last)"
  tail -n 120 "$HOME/marsa0-watch.log"
  echo "--- packets to and from port 18053 (newest last)"
  tail -n 80 "$HOME/tcpdump.log"
  echo "--- the journal of resolved, networkd, and the unit"
  sudo journalctl -u systemd-resolved -u systemd-networkd -u marsa-dns-link --since "-3min" --no-pager | grep -iE 'marsa0|link [0-9]+|dns|flush|revert|manag|started|stopp' | tail -n 150
  sudo ss -H -ltnp | grep -E ':(80|443|18019|18053|18080|18443)\b'
  cat "$PORT_START"
}
trap 'code=$?; [ "$code" -eq 0 ] || diagnose; exit "$code"' EXIT

# What `resolvectl status marsa0` shows, every 0.25 s, into a log for the
# diagnosis: a failed check deletes the unit before the script can look.
watch_marsa0() {
  # A background loop: a failed look must not end it (set -e).
  set +e
  while true; do
    printf '%s ' "$(date +%T.%N | cut -c1-12)"
    { ip -brief link show marsa0 2>&1; resolvectl status marsa0 2>&1; networkctl status marsa0 --no-pager 2>&1 | grep -E 'State|Network File'; } | tr '\n' '|'
    echo
    sleep 0.25
  done >>"$HOME/marsa0-watch.log"
}

# Runs the helper as root with the ops in "$1" (a JSON list) and prints its
# answer. The exit code is the helper's: 0 all done, 1 an op failed.
helper() {
  local request="{\"version\":1,\"ops\":$1}"
  printf '$ sudo marsa-helper apply %s\n' "$request"
  local code=0
  ANSWER=$(sudo "$BIN/marsa-helper" apply "$request") || code=$?
  echo "$ANSWER"
  return "$code"
}
# The statuses of the last answer, in order, such as "done unchanged".
statuses() { echo "$ANSWER" | grep -o '"status":"[a-z]*"' | cut -d'"' -f4 | tr '\n' ' ' | sed 's/ $//'; }
codes() { echo "$ANSWER" | grep -o '"code":"[a-z_]*"' | cut -d'"' -f4 | tr '\n' ' ' | sed 's/ $//'; }

# Waits until "$1" resolves to 127.0.0.1, 10 s at most: systemd-resolved
# may take a moment to use a new link (spike S-02b, systemd 249).
wait_for_name() {
  local i
  for i in $(seq 1 20); do
    if [ "$(getent ahostsv4 "$1" | awk 'NR==1 {print $1}')" = 127.0.0.1 ]; then
      echo "$1 resolves (try $i)"
      return 0
    fi
    sleep 0.5
  done
  return 1
}
# The ops list that trusts the PEM file "$1".
trust_ops() { jq -cn --rawfile pem "$1" '[{op: "trust_install", pem: $pem}]'; }
# Whether the system bundle holds the certificate in the PEM file "$1".
in_bundle() { grep -qF "$(sed -n 2p "$1")" "$BUNDLE"; }
# Links in /etc/ssl/certs whose target is gone.
dangling_links() { find /etc/ssl/certs -xtype l | tr '\n' ' '; }

# The first IPv4 address of "$1", or "none".
address_of() { getent ahostsv4 "$1" | awk 'NR==1 {print $1}' | grep . || echo none; }

# Waits until `curl "$@"` prints a page with "Laravel" in it, 60 s at most.
wait_for_laravel() {
  local i body
  for i in $(seq 1 60); do
    if body=$(curl -sS --max-time 10 "$@" 2>/dev/null) && grep -q Laravel <<<"$body"; then
      echo "Laravel answers (try $i)"
      return 0
    fi
    sleep 1
  done
  echo "no Laravel page from: curl $*"
  curl -sS -i --max-time 10 "$@" | head -n 30 || true
  return 1
}

step "the programs, in <app data>/bin"
mkdir -p "$BIN" "$SHIMS"
for program in marsa marsad marsa-helper marsa-shim; do
  cp "$REPO/target/release/$program" "$BIN/$program"
done
for name in php composer node npm npx; do
  ln -f "$BIN/marsa-shim" "$SHIMS/$name"
done
export PATH="$SHIMS:$BIN:$PATH"
run marsa --version
run marsa-helper version
. /etc/os-release
echo "system: $PRETTY_NAME, systemd $(systemctl --version | awk 'NR==1 {print $2}'), kernel $(uname -r)"

# Part A: the helper alone. It needs no runtime, only marsad's DNS server.

step "debug logs of systemd-resolved and systemd-networkd, kept across restarts"
for unit in systemd-resolved systemd-networkd; do
  sudo mkdir -p "/etc/systemd/system/$unit.service.d"
  printf '[Service]\nEnvironment=SYSTEMD_LOG_LEVEL=debug\n' | sudo tee "/etc/systemd/system/$unit.service.d/marsa-e2e-debug.conf" >/dev/null
done
sudo systemctl daemon-reload
sudo systemctl reset-failed systemd-resolved
sudo systemctl restart systemd-resolved
if systemctl is-active --quiet systemd-networkd; then sudo systemctl restart systemd-networkd; fi
watch_marsa0 &
WATCHER=$!
# Which link the packets to Marsa's DNS port take: lo, or marsa0 (lost).
sudo tcpdump -i any -n -l -tttt 'port 18053' >"$HOME/tcpdump.log" 2>&1 &

step "part A: before the helper"
run marsa daemon start
[ "$(cat "$PORT_START")" -gt 80 ] || fail "this machine already allows port 80; the test would prove nothing"
if resolvectl query "$SITE" >/dev/null 2>&1; then
  fail "$SITE resolves before setup"
fi

step "the helper: .test names and ports 80 and 443"
# An old global drop-in of Marsa's, as earlier builds wrote it: it goes.
sudo mkdir -p "$(dirname "$DROPIN")"
printf '# Managed by Marsa. Do not edit.\n[Resolve]\nDNS=127.0.0.1:18053\nDomains=~test\n' | sudo tee "$DROPIN"
sudo systemctl reset-failed systemd-resolved
sudo systemctl restart systemd-resolved
helper '[{"op":"resolved_install","port":18053},{"op":"ports_install"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "done done" ] || fail "expected done done, got: $(statuses)"
[ ! -e "$DROPIN" ] || fail "the old drop-in $DROPIN is still there"
run cat "$UNIT"
run cat "$SYSCTL_FILE"
[ "$(stat -c '%U:%G %a' "$UNIT")" = "root:root 644" ] || fail "wrong owner or mode on $UNIT"
[ "$(stat -c '%U:%G %a' "$SYSCTL_FILE")" = "root:root 644" ] || fail "wrong owner or mode on $SYSCTL_FILE"
[ "$(systemctl is-enabled marsa-dns-link.service)" = enabled ] || fail "the unit is not enabled"
[ "$(systemctl is-active marsa-dns-link.service)" = active ] || fail "the unit is not active"
[ "$(cat "$PORT_START")" = 80 ] || fail "ip_unprivileged_port_start is $(cat "$PORT_START"), not 80"
run resolvectl status marsa0
run ip -brief addr show marsa0
# The real DNS server of marsad answers, through systemd-resolved.
wait_for_name "$SITE" || fail "$SITE does not resolve"
run resolvectl query "$SITE"
run getent hosts "$SITE"

step "only .test names reach port 18053: a forger there cannot answer other names"
# marsad stops, so its port is free, and a stand-in that answers every name
# with 10.9.9.9 takes it, as a program that grabs the port would.
run marsa daemon stop
python3 "$REPO/spikes/s02-linux/dns-standin.py" 18053 "$HOME/forger-queries.log" spoof &
FORGER=$!
sleep 1
run sudo resolvectl flush-caches
for name in example.com github.com; do
  answer=$(address_of "$name")
  echo "$name -> $answer"
  [ "$answer" != 10.9.9.9 ] || fail "the forger on 18053 answered for $name"
done
answer=$(address_of "$SITE")
echo "$SITE -> $answer (the forger may answer .test names only)"
other=$(grep -c ' other ' "$HOME/forger-queries.log" 2>/dev/null || true)
other=${other:-0}
echo "queries outside .test at port 18053: $other"
[ "$other" = 0 ] || fail "$other queries outside .test reached port 18053"
kill "$FORGER"
wait "$FORGER" 2>/dev/null || true
run sudo resolvectl flush-caches
run marsa daemon start
wait_for_name "$SITE" || fail "$SITE does not resolve again"

step "the same request again changes nothing"
helper '[{"op":"resolved_install","port":18053},{"op":"ports_install"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "unchanged unchanged" ] || fail "expected unchanged unchanged, got: $(statuses)"

step "removal"
remove_all() {
  helper '[{"op":"ports_remove"},{"op":"resolved_remove"}]' || fail "the helper exited with $?"
  [ "$(statuses)" = "done done" ] || fail "expected done done, got: $(statuses)"
  [ ! -e "$UNIT" ] || fail "$UNIT is still there"
  ! ip link show marsa0 >/dev/null 2>&1 || fail "marsa0 is still there"
  [ "$(systemctl is-enabled marsa-dns-link.service 2>/dev/null || true)" != enabled ] || fail "the unit is still enabled"
  [ ! -e "$SYSCTL_FILE" ] || fail "$SYSCTL_FILE is still there"
  [ "$(cat "$PORT_START")" = 1024 ] || fail "ip_unprivileged_port_start is $(cat "$PORT_START"), not 1024"
  sudo resolvectl flush-caches
  if resolvectl query "$SITE" >/dev/null 2>&1; then
    fail "$SITE still resolves after the removal"
  fi
}
remove_all
helper '[{"op":"ports_remove"},{"op":"resolved_remove"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "unchanged unchanged" ] || fail "expected unchanged unchanged, got: $(statuses)"

step "the install five more times, each from an old drop-in and a restart"
# Run 43603392848 failed once here on 22.04 (the check saw another server
# list on marsa0). Each round: the old drop-in, a restart, the install, the
# name, and the removal.
for round in 1 2 3 4 5; do
  printf '# Managed by Marsa. Do not edit.\n[Resolve]\nDNS=127.0.0.1:18053\nDomains=~test\n' | sudo tee "$DROPIN" >/dev/null
  sudo systemctl reset-failed systemd-resolved
  sudo systemctl restart systemd-resolved
  helper '[{"op":"resolved_install","port":18053}]' || fail "round $round: the helper exited with $?"
  [ "$(statuses)" = "done" ] || fail "round $round: expected done, got: $(statuses)"
  wait_for_name "$SITE" || fail "round $round: $SITE does not resolve"
  helper '[{"op":"resolved_remove"}]' || fail "round $round: the helper exited with $?"
  [ "$(statuses)" = "done" ] || fail "round $round: expected done, got: $(statuses)"
done

step "removal reloads only Marsa's key from the other sysctl files"
helper '[{"op":"ports_install"}]' || fail "the helper exited with $?"
[ "$(cat "$PORT_START")" = 80 ] || fail "ports_install did not set 80"
# Another program's file sets the same key, and someone changed another
# setting by hand since the boot.
printf 'net.ipv4.ip_unprivileged_port_start = 100\nvm.swappiness = 60\n' | sudo tee /etc/sysctl.d/10-someone-else.conf
run sudo sysctl -w vm.swappiness=13
helper '[{"op":"ports_remove"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
[ "$(cat "$PORT_START")" = 100 ] || fail "the other file's value did not come back: $(cat "$PORT_START")"
[ "$(cat /proc/sys/vm/swappiness)" = 13 ] || fail "a setting changed by hand was reloaded: $(cat /proc/sys/vm/swappiness)"
echo "the other file's port value is back, and vm.swappiness kept its hand-set value"
sudo rm /etc/sysctl.d/10-someone-else.conf
run sudo sysctl -w net.ipv4.ip_unprivileged_port_start=1024

step "trust: a CA in the system store, and out again"
hash=$(openssl x509 -noout -hash -in "$TEST_CA")
echo "dangling links before the test: $(dangling_links)"
helper "$(trust_ops "$TEST_CA")" || fail "the helper exited with $?"
[ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
[ "$(stat -c '%U:%G %a' "$TRUST_FILE")" = "root:root 644" ] || fail "wrong owner or mode on $TRUST_FILE"
[ -e "/etc/ssl/certs/$hash.0" ] || fail "no hash link /etc/ssl/certs/$hash.0"
in_bundle "$TEST_CA" || fail "the CA is not in $BUNDLE"
run openssl verify -CAfile "$BUNDLE" "$TEST_CA"
helper "$(trust_ops "$TEST_CA")" || fail "the helper exited with $?"
[ "$(statuses)" = "unchanged" ] || fail "expected unchanged, got: $(statuses)"
helper '[{"op":"trust_remove"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
[ ! -e "$TRUST_FILE" ] || fail "$TRUST_FILE is still there"
! in_bundle "$TEST_CA" || fail "the CA is still in $BUNDLE"
[ ! -L "/etc/ssl/certs/$hash.0" ] || fail "the hash link /etc/ssl/certs/$hash.0 is still there"
[ -z "$(dangling_links)" ] || fail "dangling links after the removal: $(dangling_links)"
helper '[{"op":"trust_remove"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "unchanged" ] || fail "expected unchanged, got: $(statuses)"

step "next to another global DNS server, .test names still work"
# With the old global drop-in, .test never resolved here (spike S-02: 0 of 6).
sudo systemctl reset-failed systemd-resolved
printf '[Resolve]\nDNS=1.1.1.1\n' | sudo tee /etc/systemd/resolved.conf.d/00-someone-else.conf
sudo systemctl restart systemd-resolved
helper '[{"op":"resolved_install","port":18053}]' || fail "the helper exited with $?"
[ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
wait_for_name "$SITE" || fail "$SITE does not resolve next to DNS=1.1.1.1"
run resolvectl query example.com
helper '[{"op":"resolved_remove"}]' || fail "the helper exited with $?"
sudo rm /etc/systemd/resolved.conf.d/00-someone-else.conf
sudo systemctl reset-failed systemd-resolved
sudo systemctl restart systemd-resolved
run resolvectl query example.com
echo
kill "$WATCHER" 2>/dev/null || true
echo "PASS: part A, the helper"

# Part B: a real Laravel site, from the signed runtimes to the open page.

step "marsa install: PHP 8.4, Caddy, Composer (the signed manifest)"
run marsa install php 8.4
run marsa install caddy
run marsa install composer
run marsa runtimes
run php -v
run composer --version

step "a real Laravel project in a parked folder"
mkdir -p "$CODE"
(cd "$CODE" && run composer create-project --no-interaction --prefer-dist laravel/laravel laravel-app)
run marsa park "$CODE"
run marsa sites
run marsa services start caddy
run marsa services

step "before the helper: Caddy on its high ports"
wait_for_laravel -k --resolve "$SITE:18443:127.0.0.1" "https://$SITE:18443/"

step "the helper again, for the site"
helper '[{"op":"resolved_install","port":18053},{"op":"ports_install"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "done done" ] || fail "expected done done, got: $(statuses)"
wait_for_name "$SITE" || fail "$SITE does not resolve"

step "trust: Caddy's CA in the system store"
helper "$(trust_ops "$CA")" || fail "the helper exited with $?"
[ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"

step "after the helper: Caddy on ports 80 and 443"
# The daemon picks the ports at every config. Phase 2: `marsa setup` asks for
# the new config itself. Here, a restart of Caddy writes it.
run marsa services restart caddy
wait_for_laravel -k "https://$SITE/"
# The certificate, checked against the system store: no -k, no --cacert.
run curl -fsS -o /dev/null --max-time 10 -w 'https://%{remote_ip}:%{remote_port} -> %{http_code}\n' "https://$SITE/"
redirect=$(curl -sS -o /dev/null --max-time 10 -w '%{http_code} %{redirect_url}' "http://$SITE/")
echo "http://$SITE/ -> $redirect"
[ "$redirect" = "308 https://$SITE/" ] || fail "expected a 308 to https://$SITE/"
listeners=$(sudo ss -H -ltnp | grep '"caddy"' | awk '{print $4}' | sort | tr '\n' ' ')
echo "caddy listens on: $listeners"
case "$listeners" in
  *"127.0.0.1:443 "*"127.0.0.1:80 "*) ;;
  *) fail "caddy does not listen on 127.0.0.1:80 and :443" ;;
esac
if grep -vq '^127\.0\.0\.1:' <<<"$(tr ' ' '\n' <<<"$listeners" | grep .)"; then
  fail "caddy listens on an address that is not 127.0.0.1"
fi

step "removal, with the site running"
remove_all
helper '[{"op":"trust_remove"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "done" ] || fail "expected done, got: $(statuses)"
if openssl verify -CAfile "$BUNDLE" "$CA" >/dev/null 2>&1; then
  fail "the system store still trusts Caddy's CA"
fi
[ -z "$(dangling_links)" ] || fail "dangling links after the removal: $(dangling_links)"

step "stop"
run marsa daemon stop
echo
echo "PASS: Linux end to end, phase 1"
