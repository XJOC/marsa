#!/usr/bin/env bash
# Spike S-02c: on systemd 249 (Ubuntu 22.04), the first .test lookup after a
# restart of resolved took about 30 s with the DNS server 127.0.0.1:18053 on
# marsa0 (run 43605058794). resolved 249 binds a link's DNS socket to the
# link during connect() unless the server's address is a known link address
# (v250 adds an exception for localhost), so the packets leave through the
# dummy link. Two ways to keep 127.0.0.1 and avoid it:
#   addr:  127.0.0.1/32 also on marsa0, so resolved finds it as a link address
#   route: a local route for 127.0.0.1 on marsa0 in the local table
# Each variant: five rounds of (restart resolved, restart the unit, time the
# first lookup), the interface the packets took, and side effects on lo.
# Runs only on a throwaway runner (sudo).
set -uo pipefail
[ "${CI:-}" = true ] || { echo "CI only" >&2; exit 2; }

ROOT=$(cd "$(dirname "$0")" && pwd)
OUT=${OUT:-$PWD/s02c-out}
mkdir -p "$OUT"
SUMMARY=$OUT/summary.txt
: >"$SUMMARY"
PORT=18053
SITE=laravel-app.test
IP=$(command -v ip)
RESOLVECTL=$(command -v resolvectl)
UNIT=/etc/systemd/system/marsa-s02c.service
SYSTEMD=$(systemctl --version | awk 'NR==1 {print $2}')

result() { printf '%-6s %-58s %s\n' "$1" "$2" "$3" | tee -a "$SUMMARY"; }
run() { printf '$ %s\n' "$*"; "$@"; echo "[exit $?]"; }

. /etc/os-release
result T0 "os / systemd / kernel" "$PRETTY_NAME / $SYSTEMD / $(uname -r)"

python3 "$ROOT/dns-standin.py" "$PORT" "$OUT/queries.log" >>"$OUT/standin.out" 2>&1 &
sudo tcpdump -i any -n -l 'port 18053' >"$OUT/tcpdump.log" 2>&1 &
sleep 2

# Writes the unit of M4-18h with "$1" (lines) after the address line.
write_unit() {
  sudo tee "$UNIT" >/dev/null <<UNIT
[Unit]
After=systemd-resolved.service
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=-$IP link add marsa0 type dummy
ExecStart=$IP link set marsa0 up
ExecStart=-$IP addr add 169.254.53.53/32 dev marsa0
$1
ExecStart=$RESOLVECTL default-route marsa0 false
ExecStart=$RESOLVECTL domain marsa0 ~test
ExecStart=-$RESOLVECTL llmnr marsa0 no
ExecStart=-$RESOLVECTL mdns marsa0 no
ExecStart=-$RESOLVECTL dnsovertls marsa0 no
ExecStart=$RESOLVECTL dns marsa0 127.0.0.1:$PORT
ExecStopPost=-$IP link del marsa0
[Install]
WantedBy=multi-user.target
UNIT
  sudo systemctl daemon-reload
}

# Seconds until resolved answers for the site, 60 at most.
first_lookup() {
  local start end
  start=$(date +%s.%N)
  timeout 60 "$RESOLVECTL" query --cache=no "$SITE" >/dev/null 2>&1 || { echo "none"; return; }
  end=$(date +%s.%N)
  awk -v a="$start" -v b="$end" 'BEGIN {printf "%.2f", b - a}'
}

for variant in none addr route; do
  case $variant in
    none) extra="" ;;
    addr) extra="ExecStart=-$IP addr add 127.0.0.1/32 dev marsa0 scope host" ;;
    route) extra="ExecStart=-$IP route add local 127.0.0.1/32 dev marsa0 table local metric 1024" ;;
  esac
  echo; echo "===== variant $variant"
  write_unit "$extra"
  run sudo systemctl enable --now marsa-s02c.service
  run resolvectl status marsa0
  run "$IP" route show table local dev marsa0
  mark=$(wc -l <"$OUT/tcpdump.log")
  times=""
  for round in 1 2 3 4 5; do
    sudo systemctl reset-failed systemd-resolved
    sudo systemctl restart systemd-resolved
    sudo systemctl restart marsa-s02c.service
    times="$times $(first_lookup)"
  done
  result "$variant" "first lookup after restarts, 5 rounds (s)" "$times"
  # The same right after the unit only, without a restart of resolved.
  times=""
  for round in 1 2 3; do
    sudo systemctl restart marsa-s02c.service
    times="$times $(first_lookup)"
  done
  result "$variant" "first lookup after a restart of the unit only (s)" "$times"
  sleep 1
  on_marsa0=$(tail -n +"$mark" "$OUT/tcpdump.log" | grep -c '^[0-9:. -]*marsa0' || true)
  on_lo=$(tail -n +"$mark" "$OUT/tcpdump.log" | grep -c '^[0-9:. -]*lo ' || true)
  result "$variant" "packets on marsa0 / on lo" "$on_marsa0 / $on_lo"
  result "$variant" "ping 127.0.0.1 with the unit running" "$(ping -c1 -W1 127.0.0.1 >/dev/null 2>&1 && echo ok || echo FAILS)"
  run sudo systemctl disable --now marsa-s02c.service
  result "$variant" "after the stop: local route 127.0.0.1 on lo" "$("$IP" route show table local | grep -c '^local 127.0.0.1 dev lo')"
  result "$variant" "after the stop: ping 127.0.0.1" "$(ping -c1 -W1 127.0.0.1 >/dev/null 2>&1 && echo ok || echo FAILS)"
  result "$variant" "after the stop: marsa0" "$("$IP" link show marsa0 >/dev/null 2>&1 && echo present || echo gone)"
done
sudo rm -f "$UNIT"
sudo systemctl daemon-reload
echo
cat "$SUMMARY"
