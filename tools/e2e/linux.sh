#!/usr/bin/env bash
# End to end on Linux: from the built programs to an open Laravel page.
# Task M4-18f, the Linux side of the Phase 1 gate.
#
# It downloads runtimes, creates a Laravel project, and changes the system
# with sudo (systemd-resolved config and a sysctl). Run it only on a machine
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
DROPIN=/etc/systemd/resolved.conf.d/marsa.conf
SYSCTL_FILE=/etc/sysctl.d/60-marsa.conf
PORT_START=/proc/sys/net/ipv4/ip_unprivileged_port_start
CA=$DATA/caddy/data/caddy/pki/authorities/marsa/root.crt

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
  resolvectl status | head -n 30
  sudo ss -H -ltnp | grep -E ':(80|443|18019|18053|18080|18443)\b'
  cat "$PORT_START"
}
trap 'code=$?; [ "$code" -eq 0 ] || diagnose; exit "$code"' EXIT

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

step "before setup: Caddy on its high ports"
[ "$(cat "$PORT_START")" -gt 80 ] || fail "this machine already allows port 80; the test would prove nothing"
wait_for_laravel -k --resolve "$SITE:18443:127.0.0.1" "https://$SITE:18443/"
if resolvectl query "$SITE" >/dev/null 2>&1; then
  fail "$SITE resolves before setup"
fi

step "the helper: .test names and ports 80 and 443"
helper '[{"op":"resolved_install","port":18053},{"op":"ports_install"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "done done" ] || fail "expected done done, got: $(statuses)"
run cat "$DROPIN"
run cat "$SYSCTL_FILE"
[ "$(stat -c '%U:%G %a' "$DROPIN")" = "root:root 644" ] || fail "wrong owner or mode on $DROPIN"
[ "$(stat -c '%U:%G %a' "$SYSCTL_FILE")" = "root:root 644" ] || fail "wrong owner or mode on $SYSCTL_FILE"
[ "$(cat "$PORT_START")" = 80 ] || fail "ip_unprivileged_port_start is $(cat "$PORT_START"), not 80"
# The real DNS server of marsad answers, through systemd-resolved.
run resolvectl query "$SITE"
run getent hosts "$SITE"

step "the same request again changes nothing"
helper '[{"op":"resolved_install","port":18053},{"op":"ports_install"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "unchanged unchanged" ] || fail "expected unchanged unchanged, got: $(statuses)"

step "after setup: Caddy on ports 80 and 443"
# The daemon picks the ports at every config. Phase 2: `marsa setup` asks for
# the new config itself. Here, a restart of Caddy writes it.
run marsa services restart caddy
wait_for_laravel -k "https://$SITE/"
# The certificate, checked against Marsa's own CA (the system store is task
# M4-18c).
run curl -fsS -o /dev/null --max-time 10 --cacert "$CA" -w 'https://%{remote_ip}:%{remote_port} -> %{http_code}\n' "https://$SITE/"
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

step "removal"
helper '[{"op":"ports_remove"},{"op":"resolved_remove"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "done done" ] || fail "expected done done, got: $(statuses)"
[ ! -e "$DROPIN" ] || fail "$DROPIN is still there"
[ ! -e "$SYSCTL_FILE" ] || fail "$SYSCTL_FILE is still there"
[ "$(cat "$PORT_START")" = 1024 ] || fail "ip_unprivileged_port_start is $(cat "$PORT_START"), not 1024"
if resolvectl query "$SITE" >/dev/null 2>&1; then
  fail "$SITE still resolves after the removal"
fi
helper '[{"op":"ports_remove"},{"op":"resolved_remove"}]' || fail "the helper exited with $?"
[ "$(statuses)" = "unchanged unchanged" ] || fail "expected unchanged unchanged, got: $(statuses)"

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

step "a machine with another global DNS server: a clear refusal"
# Many restarts in a few seconds hit the unit's start limit.
sudo systemctl reset-failed systemd-resolved
printf '[Resolve]\nDNS=1.1.1.1\n' | sudo tee /etc/systemd/resolved.conf.d/00-someone-else.conf
sudo systemctl restart systemd-resolved
code=0
helper '[{"op":"resolved_install","port":18053}]' || code=$?
[ "$code" = 1 ] || fail "expected exit 1, got $code"
[ "$(codes)" = "resolved_other_dns" ] || fail "expected resolved_other_dns, got: $(codes)"
[ ! -e "$DROPIN" ] || fail "$DROPIN was left behind"
sudo rm /etc/systemd/resolved.conf.d/00-someone-else.conf
sudo systemctl reset-failed systemd-resolved
sudo systemctl restart systemd-resolved
run resolvectl query example.com

step "stop"
run marsa daemon stop
echo
echo "PASS: Linux end to end, phase 1"
