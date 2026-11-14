#!/usr/bin/env bash
# Spike S-02b: can `.test` names, and only them, reach Marsa's DNS server?
#
# The global drop-in of task M4-18a sends every name to 127.0.0.1:18053, so
# a program that holds the port can forge every answer (review of PR #88).
# This tries two ways that route `.test` alone:
#
#   - a link of its own, `marsa0` (dummy), with `~test` and no default
#     route, set with resolvectl, with a root unit, or with systemd-networkd;
#   - the DNS delegate file of systemd 258 and later.
#
# Each case ends with a forger on 18053 that answers every name with
# 10.9.9.9: `example.com` must not get that answer.
#
# It changes the system with sudo. Run it only on a machine that is thrown
# away afterwards (a GitHub runner, or the Fedora container of the workflow).
set -u

if [ "${CI:-}" != "true" ] && [ "${MARSA_SPIKE_DISPOSABLE:-}" != "1" ]; then
  echo "This spike changes system files. It runs only with CI=true (a runner)." >&2
  exit 2
fi

ROOT=$(cd "$(dirname "$0")" && pwd)
OUT=${OUT:-$PWD/results}
mkdir -p "$OUT"
SUMMARY=$OUT/summary.txt
: >"$SUMMARY"
QUERY_LOG=$OUT/dns-queries.log
: >"$QUERY_LOG"

PORT=18053
SITE=laravel-app.test
OTHER_NAMES="example.com example.org github.com kernel.org wikipedia.org"
SYSTEMD=$(systemctl --version | awk 'NR==1 {print $2}')
IP=$(PATH=/usr/sbin:/usr/bin:/sbin:/bin command -v ip)
RESOLVECTL=$(PATH=/usr/sbin:/usr/bin:/sbin:/bin command -v resolvectl)
# L1 showed it: systemd-resolved ignores a link with no address.
LINK_ADDRESS=169.254.53.53/32

section() { printf '\n\n===== %s =====\n' "$*"; }
run() {
  printf '$ %s\n' "$*"
  "$@"
  local rc=$?
  printf '[exit %d]\n' "$rc"
  return "$rc"
}
result() { printf '%-4s %-60s %s\n' "$1" "$2" "$3" | tee -a "$SUMMARY"; }

DNS_PID=""
start_dns() {
  stop_dns
  python3 "$ROOT/dns-standin.py" "$PORT" "$QUERY_LOG" ${1:-} >>"$OUT/dns-standin.out" 2>&1 &
  DNS_PID=$!
  sleep 1
}
stop_dns() {
  [ -n "$DNS_PID" ] || return 0
  kill "$DNS_PID" 2>/dev/null
  wait "$DNS_PID" 2>/dev/null
  DNS_PID=""
}
restart_resolved() {
  sudo systemctl reset-failed systemd-resolved 2>/dev/null
  run sudo systemctl restart systemd-resolved
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    resolvectl status >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  return 1
}
# The first address `getent` gives for `$1`, or "none".
address_of() { getent ahostsv4 "$1" 2>/dev/null | awk 'NR==1 {print $1}' | grep . || echo none; }
# How many queries for names outside .test reached our server while the
# five other names were looked up fresh.
leaked_queries() {
  sudo resolvectl flush-caches
  local before name
  before=$(grep -c ' other ' "$QUERY_LOG")
  for name in $OTHER_NAMES; do
    resolvectl query "$name" >/dev/null 2>&1
  done
  echo $(($(grep -c ' other ' "$QUERY_LOG") - before))
}
other_names_work() {
  sudo resolvectl flush-caches
  local name failures=0
  for name in $OTHER_NAMES; do
    resolvectl query "$name" >/dev/null 2>&1 || failures=$((failures + 1))
  done
  echo "$failures failed of 5"
}
# The verdict of one way: `.test`, other names, leaks, then the forger.
judge() {
  local tag=$1 way=$2
  start_dns
  sudo resolvectl flush-caches
  run resolvectl query "$SITE"
  result "$tag" "$way: $SITE" "$(address_of "$SITE")"
  result "$tag" "$way: other names" "$(other_names_work)"
  result "$tag" "$way: queries outside .test at our server" "$(leaked_queries)"
  start_dns spoof
  sudo resolvectl flush-caches
  result "$tag" "$way, a forger on $PORT: example.com" "$(address_of example.com) (10.9.9.9 = forged)"
  result "$tag" "$way, a forger on $PORT: $SITE" "$(address_of "$SITE")"
  start_dns
}

. /etc/os-release
section "T0 the system"
run uname -srm
echo "os: ${PRETTY_NAME:-?}, systemd $SYSTEMD, ip at $IP"
run systemctl is-active systemd-resolved systemd-networkd NetworkManager
run networkctl list --no-pager
run resolvectl status
result T0 "os / systemd" "${PRETTY_NAME:-?} / $SYSTEMD"
result T0 "systemd-networkd, NetworkManager" "$(systemctl is-active systemd-networkd), $(systemctl is-active NetworkManager)"

# ----------------------------------------------------- G: the global drop-in
section "G the global drop-in of M4-18a, for comparison"
run sudo mkdir -p /etc/systemd/resolved.conf.d
printf '# Managed by Marsa. Do not edit.\n[Resolve]\nDNS=127.0.0.1:%s\nDomains=~test\n' "$PORT" | sudo tee /etc/systemd/resolved.conf.d/marsa.conf
restart_resolved
judge G "global drop-in"
run sudo rm /etc/systemd/resolved.conf.d/marsa.conf
restart_resolved

# ------------------------------------------- L: marsa0 set with resolvectl
section "L1 a dummy link, with no address"
run sudo "$IP" link add marsa0 type dummy
run sudo "$IP" link set marsa0 up
run "$IP" -d link show marsa0
run resolvectl status marsa0
set_link() {
  run sudo "$RESOLVECTL" dns marsa0 "127.0.0.1:$PORT"
  run sudo "$RESOLVECTL" domain marsa0 '~test'
  run sudo "$RESOLVECTL" default-route marsa0 false
  run resolvectl status marsa0
}
set_link
result L1 "resolvectl dns/domain/default-route on marsa0" "exit codes above; scopes: $(resolvectl status marsa0 | awk -F: '/Current Scopes/ {print $2}' | xargs)"
judge L1 "marsa0, no address"

section "L1b the same link with a link-local address"
run sudo "$IP" addr add "$LINK_ADDRESS" dev marsa0
run resolvectl status marsa0
judge L1b "marsa0, 169.254.53.53/32"

section "L3 next to another global DNS= line (S-02 T2b)"
printf '[Resolve]\nDNS=1.1.1.1\n' | sudo tee /etc/systemd/resolved.conf.d/00-user.conf
restart_resolved
run resolvectl status marsa0
result L3 "marsa0 settings after a restart of resolved" "$(resolvectl dns marsa0 | cut -d: -f2- | xargs) / $(resolvectl domain marsa0 | cut -d: -f2- | xargs)"
ok=0
for i in 1 2 3 4 5 6; do
  sudo resolvectl flush-caches
  resolvectl query "$SITE" >/dev/null 2>&1 && ok=$((ok + 1))
  resolvectl query example.com >/dev/null 2>&1
done
result L3 "with DNS=1.1.1.1 also global: $SITE" "$ok of 6 tries"
run sudo rm /etc/systemd/resolved.conf.d/00-user.conf
restart_resolved

section "L4 persistence: a root unit with no shell"
run sudo "$IP" link del marsa0
sudo tee /etc/systemd/system/marsa-dns-link.service <<UNIT
[Unit]
Description=Marsa: send .test names to marsad (spike S-02b)
Wants=systemd-resolved.service
After=systemd-resolved.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=-$IP link add marsa0 type dummy
ExecStart=$IP link set marsa0 up
ExecStart=-$IP addr add $LINK_ADDRESS dev marsa0
ExecStart=$RESOLVECTL dns marsa0 127.0.0.1:$PORT
ExecStart=$RESOLVECTL domain marsa0 ~test
ExecStart=$RESOLVECTL default-route marsa0 false
ExecStop=$IP link del marsa0

[Install]
WantedBy=multi-user.target
UNIT
run sudo systemctl daemon-reload
run sudo systemctl enable --now marsa-dns-link.service
run systemctl status marsa-dns-link.service --no-pager
judge L4 "marsa0 from the unit"
run "$IP" route show table all dev marsa0
result L4 "routes that the address adds" "$("$IP" route show table all dev marsa0 | tr '\n' ';')"
restart_resolved
result L4 "after a restart of resolved: marsa0 dns / domain" "$(resolvectl dns marsa0 | cut -d: -f2- | xargs) / $(resolvectl domain marsa0 | cut -d: -f2- | xargs)"
run sudo systemctl restart marsa-dns-link.service
result L4 "after a restart of the unit: $SITE" "$(sudo resolvectl flush-caches; address_of "$SITE")"
run sudo systemctl stop marsa-dns-link.service
result L4 "after the unit stops: marsa0, $SITE" "$("$IP" link show marsa0 >/dev/null 2>&1 && echo present || echo gone), $(sudo resolvectl flush-caches; address_of "$SITE")"
run sudo systemctl disable marsa-dns-link.service
run sudo rm /etc/systemd/system/marsa-dns-link.service
run sudo systemctl daemon-reload

section "L6 the same with systemd-networkd files"
sudo mkdir -p /etc/systemd/network
printf '[NetDev]\nName=marsa0\nKind=dummy\n' | sudo tee /etc/systemd/network/50-marsa0.netdev
printf '[Match]\nName=marsa0\n\n[Link]\nRequiredForOnline=no\n\n[Network]\nAddress=%s\nDNS=127.0.0.1:%s\nDomains=~test\nDNSDefaultRoute=no\nLinkLocalAddressing=no\nConfigureWithoutCarrier=yes\n' "$LINK_ADDRESS" "$PORT" | sudo tee /etc/systemd/network/50-marsa0.network
was_networkd=$(systemctl is-active systemd-networkd)
if [ "$was_networkd" = active ]; then
  run sudo networkctl reload
else
  run sudo systemctl start systemd-networkd
fi
sleep 3
run networkctl status marsa0 --no-pager
run resolvectl status marsa0
judge L6 "marsa0 from networkd (it was $was_networkd)"
run sudo rm /etc/systemd/network/50-marsa0.netdev /etc/systemd/network/50-marsa0.network
run sudo networkctl reload
run sudo "$IP" link del marsa0
[ "$was_networkd" = active ] || run sudo systemctl stop systemd-networkd systemd-networkd.socket

# ------------------------------------------------- D: the delegate (258 on)
if [ "$SYSTEMD" -ge 258 ]; then
  section "L7 the DNS delegate file (systemd $SYSTEMD)"
  sudo mkdir -p /etc/systemd/dns-delegate.d
  printf '# Managed by Marsa. Do not edit.\n[Delegate]\nDNS=127.0.0.1:%s\nDomains=~test\nDefaultRoute=no\n' "$PORT" | sudo tee /etc/systemd/dns-delegate.d/marsa.dns-delegate
  restart_resolved
  judge L7 "delegate"
  run sudo rm /etc/systemd/dns-delegate.d/marsa.dns-delegate
  restart_resolved
  result L7 "after the removal: $SITE" "$(sudo resolvectl flush-caches; address_of "$SITE")"
else
  result L7 "DNS delegate" "not tested: systemd $SYSTEMD"
fi

# NetworkManager last: starting it may change the runner's own network.
# --------------------------------------------- L5: NetworkManager next to it
section "L5 NetworkManager next to marsa0"
if ! systemctl list-unit-files NetworkManager.service >/dev/null 2>&1 || [ "$(systemctl is-active NetworkManager)" != active ]; then
  if [ -f /etc/debian_version ]; then
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q network-manager >/dev/null 2>&1
  else
    sudo dnf install -y -q NetworkManager >/dev/null 2>&1
  fi
fi
run sudo "$IP" link add marsa0 type dummy
run sudo "$IP" link set marsa0 up
run sudo "$IP" addr add "$LINK_ADDRESS" dev marsa0
set_link
run sudo systemctl start NetworkManager
sleep 8
run nmcli device status
result L5 "NetworkManager's view of marsa0" "$(nmcli -t -f DEVICE,STATE device status 2>/dev/null | grep '^marsa0:' | cut -d: -f2 || echo unknown)"
run resolvectl status marsa0
judge L5 "marsa0 with NetworkManager running"
run sudo nmcli general reload
sleep 3
result L5 "after nmcli general reload: marsa0 dns / domain" "$(resolvectl dns marsa0 | cut -d: -f2- | xargs) / $(resolvectl domain marsa0 | cut -d: -f2- | xargs)"
run sudo "$IP" link del marsa0

stop_dns
section "summary"
cat "$SUMMARY"
