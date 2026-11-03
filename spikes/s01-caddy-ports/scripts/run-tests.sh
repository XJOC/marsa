#!/bin/bash
# S-01 spike: can an unprivileged process bind low ports on macOS?
# Part 1: facts, T0-T5, T12 on real holders. Part 2: real Caddy on high loopback
# ports (the targets of the pf redirect, option B), T6-T11, T12 on our own holders.
# No sudo, no writes outside this folder. Every listener is loopback except the
# wildcard sockets of T5 (closed at once) and T11 (10 s, no accept): owner-approved.
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PROBE=$ROOT/target/release/port-probe
OUT=$ROOT/results/$(date +%Y%m%d-%H%M%S)
SUMMARY=$OUT/summary.txt
CADDY_DIRS=("$HOME/Library/Application Support/Caddy" "$HOME/.config/caddy")
CADDY=$ROOT/bin/caddy
ADMIN=127.0.0.1:2101       # not 2019 (Herd, Valet) and not 2106 (spike S-06)
DATA=$ROOT/data            # Caddy storage and XDG dirs; holds the CA key, never copied
HP=18080 HS=18443          # high loopback ports: the pf redirect targets (80 and 443)
CPID="" HPID=""            # the only processes this script starts
[ -n "$ROOT" ] && rm -rf "$DATA"   # fresh Caddy state (CA, certs) for every run
mkdir -p "$OUT" "$DATA/logs" "$DATA/xdg-data" "$DATA/xdg-config"
[ -x "$PROBE" ] || { echo "build first: cargo build --release" >&2; exit 1; }

# Results must hold no user name and no home path.
scrub() { sed "s|$PROBE|port-probe|g; s|$HOME|~|g; s|[[:<:]]$USER[[:>:]]|<me>|g"; }
# run <case> <cmd...>: append "$ cmd", the output, and the exit code to <case>.txt.
# Sets LAST (output) and RC.
run() {
  local file=$1; shift
  LAST=$("$@" 2>&1); RC=$?
  printf '$ %s\n%s\nexit=%s\n\n' "$*" "$LAST" "$RC" | scrub >>"$OUT/$file.txt"
}
row() { printf '%-5s %-30s %s\n' "$1" "$2" "$3" | scrub | tee -a "$SUMMARY"; }
short() {
  case $LAST in
    OK*) echo "OK" ;;
    CONNECTED*) echo "CONNECTED" ;;
    REFUSED*) echo "REFUSED (nothing listens)" ;;
    *raw_os_error=13*) echo "FAIL EACCES(13)" ;;
    *raw_os_error=48*) echo "FAIL EADDRINUSE(48)" ;;
    *) echo "OTHER: $LAST" ;;
  esac
}
probe() { # probe <case> <label> <bind|connect> <addr:port>
  run "$1" "$PROBE" "$3" "$4"; row "$1" "$2 $4" "$(short)"
}
snapshot() { for d in "${CADDY_DIRS[@]}"; do ls -la "$d" 2>&1; done | scrub; }
# Only the LISTEN lines of one port; the full table has every connection on the machine.
netstat_port() { # proto local-address process:pid
  /usr/sbin/netstat -anv -p tcp | awk -v p="\\.$1\$" '$6=="LISTEN" && $4 ~ p {
    for (i = 7; i <= NF; i++) if ($i ~ /^[A-Za-z].*:[0-9]+$/) { print $1, $4, $i; break } }'
}

cleanup() {
  [ -n "$CPID" ] && kill "$CPID" 2>/dev/null
  [ -n "$HPID" ] && kill "$HPID" 2>/dev/null
  wait 2>/dev/null
  for f in "$DATA"/logs/*.log; do [ -f "$f" ] && scrub <"$f" >"$OUT/$(basename "$f")"; done
}
trap cleanup EXIT INT TERM
printf '%-5s %-30s %s\n' CASE WHAT RESULT >"$SUMMARY"; cat "$SUMMARY"
snapshot >"$OUT/state-before.txt"

# 1. Machine facts (read only).
for c in "sw_vers" "uname -m" "sysctl net.inet.ip.portrange" \
  "/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate"; do
  # shellcheck disable=SC2086 # word splitting is the point: command + args
  run facts $c
done
run holders-80-443 /usr/sbin/lsof -nP -iTCP:80 -iTCP:443 -sTCP:LISTEN

# T0. Are 80 and 443 free before we start?
for a in 127.0.0.1:80 127.0.0.1:443 '[::1]:80' '[::1]:443'; do probe T0 connect connect "$a"; done
# Ports 80/443 may be held by another tool (Herd). Free low ports prove the same
# kernel rule (the check is "port < 1024"), so we use them as stand-ins.
LOW=(80 443 81 444 1023)

# T1/T2. The question: bind loopback low ports as a normal user.
for p in "${LOW[@]}" 1024; do probe T1 bind bind "127.0.0.1:$p"; done
for p in 80 443 81 444; do probe T2 bind bind "[::1]:$p"; done
# T3. Control: a high port must work, or the probe is broken.
probe T3 bind bind 127.0.0.1:8080; probe T3 bind bind '[::1]:8080'
# T4. Two low ports in one process.
run T4 "$PROBE" bind 127.0.0.1:81 127.0.0.1:444
row T4 "bind 127.0.0.1:81+444" "$(printf '%s\n' "$LAST" | grep -c '^OK') of 2 OK"

# T5. Diagnostic wildcard binds, closed at once, no accept (owner-approved).
for a in 0.0.0.0:81 '[::]:81' 0.0.0.0:444 '[::]:444' 0.0.0.0:80 '[::]:80' 0.0.0.0:443; do
  probe T5 "wildcard bind" bind "$a"
done

# T12: what can each method see on a held port? detect <label> <host> <port> [bind]
detect() {
  local label=$1 host=$2 port=$3 pid
  run "T12-$label" "$PROBE" connect "$host:$port"; row T12 "$label connect $host:$port" "$(short)"
  if [ "${4:-}" = bind ]; then
    run "T12-$label" "$PROBE" bind "$host:$port"; row T12 "$label bind $host:$port" "$(short)"
  fi
  run "T12-$label" netstat_port "$port"
  row T12 "$label netstat -anv" "$(printf '%s\n' "$LAST" | awk '{printf "%s%s=%s", s, $2, $3; s="; "}' | grep . || echo nothing)"
  local pids; pids=$(printf '%s\n' "$LAST" | awk '{sub(/.*:/, "", $3); print $3}' | sort -u)
  run "T12-$label" /usr/sbin/lsof -nP "-iTCP:$port" -sTCP:LISTEN
  row T12 "$label lsof" "$(printf '%s\n' "$LAST" | awk 'NR>1 {printf "%s%s pid=%s user=%s %s", s, $1, $2, $3, $9; s="; "}' | grep . || echo nothing)"
  for pid in $pids; do
    run "T12-$label" /bin/ps -p "$pid" -o user=,comm=; row T12 "$label ps -p $pid" "${LAST:-nothing}"
  done
}
nc_free() { "$PROBE" connect "$1" >/dev/null 2>&1; [ $? -ne 0 ]; }
nc_free 127.0.0.1:80 || detect herd-nginx 127.0.0.1 80          # root master, user workers
nc_free 127.0.0.1:53 || detect herd-dnsmasq 127.0.0.1 53        # other user (nobody)
nc_free 127.0.0.1:5000 || detect airplay 127.0.0.1 5000          # same user, wildcard *.5000

# ---- Part 2: real Caddy on high loopback ports -------------------------------
# mk <listen-json> <http_port> <https_port>: fill the template.
mk() {
  sed "s|__ADMIN__|$ADMIN|; s|__DATA__|$DATA|; s|__LISTEN__|$1|; s|__HTTP_PORT__|$2|; s|__HTTPS_PORT__|$3|" "$ROOT/caddy.json"
}
printf '{"admin":{"listen":"%s"},"storage":{"module":"file_system","root":"%s"}}' "$ADMIN" "$DATA" >"$DATA/empty.json"
start_caddy() { # start_caddy <config> <case>: sets CPID; 0 when serving, 1 when it died or timed out
  # A plain background command, so $! is Caddy itself (a function wrapper would fork a
  # subshell, and killing it would leave Caddy running).
  XDG_DATA_HOME="$DATA/xdg-data" XDG_CONFIG_HOME="$DATA/xdg-config" \
    "$CADDY" run --config "$1" >"$DATA/logs/$2.caddy.log" 2>&1 &
  CPID=$!
  local i
  for i in $(seq 1 60); do
    kill -0 "$CPID" 2>/dev/null || { CPID=""; return 1; }
    grep -q 'serving initial configuration' "$DATA/logs/$2.caddy.log" && return 0
    sleep 0.1
  done
  return 1
}
stop_caddy() { [ -n "$CPID" ] && kill "$CPID" 2>/dev/null; wait "$CPID" 2>/dev/null; CPID=""; }
start_hold() { # start_hold <case> <addr> <seconds>: sets HPID once the socket is bound
  "$PROBE" hold "$2" --seconds "$3" >"$DATA/logs/$1.hold.log" 2>&1 &
  HPID=$!
  local i
  for i in $(seq 1 20); do grep -q '^OK holding' "$DATA/logs/$1.hold.log" && return 0; sleep 0.1; done
  return 1
}
stop_hold() { kill "$HPID" 2>/dev/null; wait "$HPID" 2>/dev/null; HPID=""; }
load() { # load <case> <listen-json> <http_port> <https_port>: POST /load, as marsad does
  mk "$2" "$3" "$4" >"$DATA/$1.json"
  run "$1" curl -sS --max-time 20 -w '\nHTTP %{http_code}' -X POST -H 'Content-Type: application/json' \
    --data-binary "@$DATA/$1.json" "http://$ADMIN/load"
  HTTP=${LAST##*HTTP }
}
reset() { curl -sS -X POST -H 'Content-Type: application/json' --data-binary "@$DATA/empty.json" "http://$ADMIN/load" >/dev/null; }
load_row() { # the Caddy error text, verbatim, or OK
  if [ "$HTTP" = 200 ]; then row "$1" "$2" "load OK (HTTP 200)"
  else row "$1" "$2" "HTTP $HTTP $(printf '%s' "$LAST" | sed -n 's/.*"error": *"\(.*\)"}.*/\1/p')"; fi
}
get() { # get <case> <scheme> <port> <ip>: request app.test without DNS or hosts
  if [ "$2" = https ]; then
    run "$1" curl -sS -i --max-time 5 --resolve "app.test:$3:$4" --cacert "$DATA/pki/authorities/local/root.crt" "https://app.test:$3/"
  else run "$1" curl -sS -i --max-time 5 --resolve "app.test:$3:$4" "http://app.test:$3/"; fi
}
sockets() { # sockets <case> <pid>: every inet socket of the process; flags non-loopback
  run "$1" /usr/sbin/lsof -nP -a -p "$2" -i
  local all bad
  all=$(printf '%s\n' "$LAST" | awk 'NR>1 && $9 !~ /->/ {print $8" "$9}' | sort -u)
  bad=$(printf '%s\n' "$all" | grep -vE ' (127\.0\.0\.1|\[::1\]):' | grep .)
  row "$1" "caddy sockets" "$(printf '%s' "$all" | paste -sd, - | sed 's/,/, /g')"
  row "$1" "caddy non-loopback sockets" "${bad:-none}"
}

for p in $HP $HS 18081 2101; do
  nc_free "127.0.0.1:$p" && nc_free "[::1]:$p" || { row ABORT "port $p is busy" "skipping Caddy cases"; exit 1; }
done
if [ ! -x "$CADDY" ]; then "$ROOT/scripts/fetch-caddy.sh" >"$OUT/fetch-caddy.txt" 2>&1 || { row ABORT "fetch-caddy.sh" failed; exit 1; }; scrub <"$OUT/fetch-caddy.txt" >"$OUT/fetch-caddy.txt.s" && mv "$OUT/fetch-caddy.txt.s" "$OUT/fetch-caddy.txt"; fi
run caddy-version "$CADDY" version; row INFO "caddy" "$LAST"

# T6. Caddy on 127.0.0.1, high ports; the redirect listener is Caddy's own.
mk "\"127.0.0.1:$HS\"" $HP $HS >"$DATA/t6.json"
if start_caddy "$DATA/t6.json" T6; then
  row T6 "caddy run 127.0.0.1:$HS" "starts"
  sockets T6 "$CPID"
  get T6 https $HS 127.0.0.1; row T6 "https 127.0.0.1:$HS" "$(printf '%s' "$LAST" | sed -n '1p;$p' | paste -sd' ' -)"
  get T6 http $HP 127.0.0.1; row T6 "http redirect :$HP" "$(printf '%s' "$LAST" | grep -iE '^(HTTP|Location)' | tr -d '\r' | paste -sd' ' -)"
  stop_caddy
else row T6 "caddy run 127.0.0.1:$HS" "FAILED: $(grep -h '"level":"error"' "$DATA/logs/T6.caddy.log" | head -1)"; fi
# T6b. Same, but Caddy's default http_port (80): the redirect listener wants 127.0.0.1:80.
mk "\"127.0.0.1:$HS\"" 80 $HS >"$DATA/t6b.json"
if start_caddy "$DATA/t6b.json" T6b; then row T6b "caddy run, http_port 80" "starts"; stop_caddy
else row T6b "caddy run, http_port 80" "$(sed -n 's/.*"msg":"\(loading initial config[^"]*\)".*/\1/p' "$DATA/logs/T6b.caddy.log" | head -1)"; fi

# T7. Same on ::1.
mk "\"[::1]:$HS\"" $HP $HS >"$DATA/t7.json"
if start_caddy "$DATA/t7.json" T7; then
  row T7 "caddy run [::1]:$HS" "starts"
  sockets T7 "$CPID"
  get T7 https $HS '[::1]'; row T7 "https [::1]:$HS" "$(printf '%s' "$LAST" | sed -n '1p;$p' | paste -sd' ' -)"
  stop_caddy
else row T7 "caddy run [::1]:$HS" "FAILED: $(grep -h '"level":"error"' "$DATA/logs/T7.caddy.log" | head -1)"; fi

# T8. Five start/stop cycles with no pause, one request each.
ok=0
for i in 1 2 3 4 5; do
  if start_caddy "$DATA/t6.json" "T8-$i"; then
    get "T8-$i" https $HS 127.0.0.1; printf '%s' "$LAST" | grep -q '^ok' && ok=$((ok + 1))
    stop_caddy
  fi
done
row T8 "5 start/stop cycles, no pause" "$ok of 5 served a request"

# T9-T11 use POST /load against one running Caddy that starts with no apps.
start_caddy "$DATA/empty.json" admin || { row ABORT "admin-only caddy" "did not start"; exit 1; }
# T9. Another program holds the address Caddy wants.
start_hold T9 "127.0.0.1:$HS" 60; load T9 "\"127.0.0.1:$HS\"" $HP $HS; load_row T9 "hold 127.0.0.1:$HS, load"
detect T9-hold 127.0.0.1 $HS bind; stop_hold; reset
start_hold T9b "127.0.0.1:$HP" 60; load T9b "\"127.0.0.1:$HS\"" $HP $HS; load_row T9b "hold 127.0.0.1:$HP (redirect), load"
stop_hold; reset
# T10. The other program is on ::1, Caddy on 127.0.0.1: separate address families.
start_hold T10 "[::1]:$HS" 60; load T10 "\"127.0.0.1:$HS\"" $HP $HS; load_row T10 "hold [::1]:$HS, load 127.0.0.1" 
detect T10-v4 127.0.0.1 $HS; detect T10-v6 '[::1]' $HS; stop_hold; reset
# T11. A wildcard holder on 18080 (10 s, no accept), then loopback binds on the same port.
start_hold T11 0.0.0.0:18080 10
run T11 "$PROBE" bind 127.0.0.1:18080; row T11 "wildcard hold, then std bind 127.0.0.1" "$(short)"
load T11 '"127.0.0.1:18080"' 18081 18080; load_row T11 "wildcard hold, then caddy load"
get T11 https 18080 127.0.0.1; row T11 "https 127.0.0.1:18080" "$(printf '%s' "$LAST" | sed -n '1p;$p' | paste -sd' ' -)"
detect T11 127.0.0.1 18080 bind
stop_hold
stop_caddy

snapshot >"$OUT/state-after.txt"
cmp -s "$OUT/state-before.txt" "$OUT/state-after.txt" && row STATE "Caddy user dirs" "unchanged" || row STATE "Caddy user dirs" "CHANGED"
# Nothing of ours may stay alive, and the ports we used must be free again.
left=""; for p in $HP $HS 18081 2101; do nc_free "127.0.0.1:$p" && nc_free "[::1]:$p" || left="$left $p"; done
if [ -n "$left" ]; then row END "ports $HP $HS 18081 2101" "BUSY:$left"; else row END "ports $HP $HS 18081 2101" "free"; fi
row END "caddy processes of this folder" "$(pgrep -f "$CADDY run" | wc -l | tr -d ' ') alive"
row END "ports 80 443 as before" "$(nc_free 127.0.0.1:80 && echo free || echo 'held by Herd, as before')"
echo "results: $OUT"
