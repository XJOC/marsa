#!/usr/bin/env bash
# Spike S-02: what marsa-helper needs on Linux (ports 80 and 443, `.test`
# names through systemd-resolved, the CA in the trust stores, and pkexec).
#
# It changes the system with sudo: resolved config, file capabilities, a
# sysctl, the CA store, and a polkit rule. Run it only on a machine that is
# thrown away afterwards (a GitHub runner, or the Fedora container of the
# workflow). It refuses to start anywhere else.
#
# Every case prints its commands, their output, and their exit codes. The
# short results go to $OUT/summary.txt.
set -u

if [ "${CI:-}" != "true" ] && [ "${MARSA_SPIKE_DISPOSABLE:-}" != "1" ]; then
  echo "This spike changes system files. It runs only with CI=true (a runner)." >&2
  exit 2
fi

ROOT=$(cd "$(dirname "$0")" && pwd)
OUT=${OUT:-$PWD/results}
# Under $HOME, not /tmp: Fedora mounts /tmp with nosuid, which would hide the
# file capability tests.
WORK=$(mktemp -d "$HOME/marsa-spike.XXXXXX")
mkdir -p "$OUT"
SUMMARY=$OUT/summary.txt
: >"$SUMMARY"

CADDY_VERSION=2.11.8
DNS_PORT=18053
SITE=laravel-app.test
DROPIN=/etc/systemd/resolved.conf.d/marsa.conf
BODY="marsa spike ok"

section() { printf '\n\n===== %s =====\n' "$*"; }
# Prints the command, runs it, and prints its exit code.
run() {
  printf '$ %s\n' "$*"
  "$@"
  local rc=$?
  printf '[exit %d]\n' "$rc"
  return "$rc"
}
result() { printf '%-4s %-58s %s\n' "$1" "$2" "$3" | tee -a "$SUMMARY"; }
now_ms() { echo $(($(date +%s%N) / 1000000)); }
have() { command -v "$1" >/dev/null 2>&1; }

. /etc/os-release
case "${ID:-} ${ID_LIKE:-}" in
  *debian* | *ubuntu*) FAMILY=debian ;;
  *fedora* | *rhel*) FAMILY=fedora ;;
  *) FAMILY=other ;;
esac
SYSTEMD=$(systemctl --version | awk 'NR==1 {print $2}')

# ---------------------------------------------------------------- T0: system
section "T0 the system"
run uname -srm
echo "os: ${PRETTY_NAME:-?} (family $FAMILY)"
echo "systemd: $SYSTEMD"
run id
run cat /proc/sys/net/ipv4/ip_unprivileged_port_start
run cat /proc/sys/fs/suid_dumpable
run findmnt -no SOURCE,FSTYPE,OPTIONS -T /
run findmnt -no SOURCE,FSTYPE,OPTIONS -T "$HOME"
for tool in setcap getcap sysctl pkexec pkaction certutil update-ca-certificates update-ca-trust resolvectl systemctl ss; do
  path=$(PATH=/usr/sbin:/usr/bin:/sbin:/bin command -v "$tool" 2>/dev/null || true)
  real=""
  [ -n "$path" ] && real=$(readlink -f "$path")
  echo "tool $tool: ${path:-MISSING} ${real:+-> $real}"
done
if [ "$FAMILY" = debian ]; then
  # `Priority` and `Task` say whether a default install has the package.
  for package in libcap2-bin libnss3-tools policykit-1 pkexec polkitd systemd-resolved ca-certificates procps; do
    echo "--- apt-cache show $package"
    apt-cache show "$package" 2>/dev/null | grep -E '^(Version|Priority|Task):' | sort -u || true
    dpkg-query -W -f='installed: ${Status}\n' "$package" 2>/dev/null || echo "installed: no"
  done
else
  for package in libcap nss-tools polkit systemd-resolved ca-certificates p11-kit-trust; do
    rpm -q "$package" 2>&1 || true
  done
fi
result T0 "os / systemd / kernel" "${PRETTY_NAME:-?} / $SYSTEMD / $(uname -r)"
result T0 "ip_unprivileged_port_start, suid_dumpable" "$(cat /proc/sys/net/ipv4/ip_unprivileged_port_start), $(cat /proc/sys/fs/suid_dumpable)"
result T0 "mount options of \$HOME" "$(findmnt -no OPTIONS -T "$HOME")"

# ------------------------------------------------------- T1: DNS before setup
section "T1 systemd-resolved before any change"
run systemctl is-active systemd-resolved
run ls -l /etc/resolv.conf
run cat /etc/resolv.conf
run resolvectl status
run systemctl show -p CanReload systemd-resolved
run resolvectl query "$SITE"
result T1 "resolved active, resolv.conf" "$(systemctl is-active systemd-resolved) / $(readlink -f /etc/resolv.conf)"
result T1 "$SITE before setup" "$(resolvectl query "$SITE" >/dev/null 2>&1 && echo resolves || echo "does not resolve")"

OTHER_NAMES="example.com example.org github.com kernel.org wikipedia.org"
# Fresh lookups of names outside .test: prints one "name rc ms" per name.
time_other_names() {
  sudo resolvectl flush-caches
  local name start rc
  for name in $OTHER_NAMES; do
    start=$(now_ms)
    resolvectl query "$name" >/dev/null 2>&1
    rc=$?
    echo "  $name rc=$rc ms=$(($(now_ms) - start))"
  done
}
total_ms() { awk -F'ms=' '{sum += $2} END {print sum}'; }

echo "--- baseline: fresh lookups outside .test"
BASE=$(time_other_names)
echo "$BASE"
result T1 "5 fresh lookups outside .test, baseline" "$(echo "$BASE" | total_ms) ms total"

# ------------------------------------------------ T2: the resolved drop-in
section "T2 resolved drop-in: DNS=127.0.0.1:$DNS_PORT and Domains=~test"
QUERY_LOG=$OUT/dns-queries.log
: >"$QUERY_LOG"
start_dns() {
  python3 "$ROOT/dns-standin.py" "$DNS_PORT" "$QUERY_LOG" >"$OUT/dns-standin.out" 2>&1 &
  DNS_PID=$!
  sleep 1
}
stop_dns() { kill "$DNS_PID" 2>/dev/null; wait "$DNS_PID" 2>/dev/null; }
restart_resolved() {
  run sudo systemctl restart systemd-resolved
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    resolvectl status >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  echo "resolved did not come back"
  return 1
}
start_dns
T2_START=$(date '+%Y-%m-%d %H:%M:%S')
run sudo mkdir -p /etc/systemd/resolved.conf.d
printf '# Managed by Marsa. Do not edit.\n[Resolve]\nDNS=127.0.0.1:%s\nDomains=~test\n' "$DNS_PORT" | sudo tee "$DROPIN"
run sudo chmod 0644 "$DROPIN"
restart_resolved
run resolvectl status
GLOBAL=$(resolvectl status | awk '/^Global/{on=1; next} /^Link /{on=0} on' | tr -s ' ' | tr '\n' ';')
result T2 "global scope after the drop-in" "$GLOBAL"
run resolvectl query "$SITE"
run resolvectl query --type=AAAA "$SITE"
run resolvectl query api.shop.test
run getent hosts "$SITE"
run getent ahostsv4 "$SITE"
result T2 "resolvectl query $SITE" "$(resolvectl query "$SITE" 2>&1 | head -1 | tr -s ' ')"
result T2 "getent hosts $SITE (glibc, as curl and browsers)" "$(getent hosts "$SITE" | tr -s ' ' || echo FAILED)"

echo "--- fresh lookups outside .test, with the drop-in and our server up"
before=$(grep -c ' other ' "$QUERY_LOG")
WITH=$(time_other_names)
echo "$WITH"
leaked=$(($(grep -c ' other ' "$QUERY_LOG") - before))
result T2 "5 fresh lookups outside .test, server up" "$(echo "$WITH" | total_ms) ms total, failures: $(echo "$WITH" | grep -vc 'rc=0')"
result T2 "queries outside .test that reached our server" "$leaked"
run tail -n 20 "$QUERY_LOG"

echo "--- our server stopped (marsad not running)"
stop_dns
DOWN=$(time_other_names)
echo "$DOWN"
start=$(now_ms)
run resolvectl query "$SITE"
site_down_ms=$(($(now_ms) - start))
result T2 "5 fresh lookups outside .test, server down" "$(echo "$DOWN" | total_ms) ms total, failures: $(echo "$DOWN" | grep -vc 'rc=0')"
result T2 "$SITE with the server down" "fails after $site_down_ms ms"
start_dns

echo "--- what resolved logged"
run sudo journalctl -u systemd-resolved --since "$T2_START" --no-pager -o cat
run sudo systemctl reload systemd-resolved
result T2 "systemctl reload systemd-resolved" "exit $? ($(systemctl show -p CanReload --value systemd-resolved))"

section "T2b the same drop-in on a machine that already has a global DNS= line"
printf '[Resolve]\nDNS=1.1.1.1\n' | sudo tee /etc/systemd/resolved.conf.d/00-user.conf
restart_resolved
run resolvectl status
ok=0
for i in 1 2 3 4 5 6; do
  sudo resolvectl flush-caches
  resolvectl query "$SITE" >/dev/null 2>&1 && ok=$((ok + 1))
  resolvectl query "example.com" >/dev/null 2>&1
done
run resolvectl query "$SITE"
run resolvectl status
result T2b "with DNS=1.1.1.1 also global: $SITE resolved" "$ok of 6 tries"
run sudo rm /etc/systemd/resolved.conf.d/00-user.conf
restart_resolved

if [ "$SYSTEMD" -ge 258 ]; then
  section "T2c systemd $SYSTEMD: a DNS delegate instead of the global drop-in"
  run sudo rm "$DROPIN"
  run sudo mkdir -p /etc/systemd/dns-delegate.d
  printf '# Managed by Marsa. Do not edit.\n[Delegate]\nDNS=127.0.0.1:%s\nDomains=~test\nDefaultRoute=no\n' "$DNS_PORT" | sudo tee /etc/systemd/dns-delegate.d/marsa.dns-delegate
  restart_resolved
  run resolvectl status
  run resolvectl query "$SITE"
  before=$(grep -c ' other ' "$QUERY_LOG")
  DELEGATE=$(time_other_names)
  echo "$DELEGATE"
  leaked=$(($(grep -c ' other ' "$QUERY_LOG") - before))
  result T2c "delegate: $SITE" "$(resolvectl query "$SITE" 2>&1 | head -1 | tr -s ' ')"
  result T2c "delegate: queries outside .test at our server" "$leaked"
  run sudo rm /etc/systemd/dns-delegate.d/marsa.dns-delegate
  printf '# Managed by Marsa. Do not edit.\n[Resolve]\nDNS=127.0.0.1:%s\nDomains=~test\n' "$DNS_PORT" | sudo tee "$DROPIN"
  restart_resolved
else
  result T2c "DNS delegate (needs systemd 258)" "not tested: systemd $SYSTEMD"
fi

# ------------------------------------------------------------ Caddy download
section "Caddy $CADDY_VERSION (official release, sha512 checked)"
case "$(uname -m)" in
  x86_64) ARCH=amd64 ;;
  aarch64) ARCH=arm64 ;;
  *) echo "unknown machine $(uname -m)"; exit 1 ;;
esac
BASE_URL=https://github.com/caddyserver/caddy/releases/download/v$CADDY_VERSION
ARCHIVE=caddy_${CADDY_VERSION}_linux_${ARCH}.tar.gz
SUMS=caddy_${CADDY_VERSION}_checksums.txt
curl -fsSL --max-time 120 -o "$WORK/$SUMS" "$BASE_URL/$SUMS" || exit 1
curl -fsSL --max-time 300 -o "$WORK/$ARCHIVE" "$BASE_URL/$ARCHIVE" || exit 1
expected=$(awk -v f="$ARCHIVE" '$2 == f {print $1}' "$WORK/$SUMS")
actual=$(sha512sum "$WORK/$ARCHIVE" | awk '{print $1}')
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  echo "FAIL: sha512 mismatch for $ARCHIVE"
  exit 1
fi
echo "sha512 OK for $ARCHIVE"
# The place `marsa install caddy` uses.
CADDY_DIR=$HOME/.local/share/marsa/runtimes/caddy/$CADDY_VERSION
mkdir -p "$CADDY_DIR"
tar -xzf "$WORK/$ARCHIVE" -C "$CADDY_DIR" caddy
CADDY=$CADDY_DIR/caddy
run "$CADDY" version
run ls -l "$CADDY"

# Writes a Caddy config for the given ports and prints its path.
caddy_config() {
  local http=$1 https=$2 file=$WORK/caddy-$1-$2.json
  cat >"$file" <<JSON
{
  "admin": { "listen": "127.0.0.1:18019" },
  "storage": { "module": "file_system", "root": "$WORK/caddy-data" },
  "apps": {
    "http": {
      "http_port": $http,
      "https_port": $https,
      "servers": {
        "app": {
          "listen": ["127.0.0.1:$https"],
          "protocols": ["h1", "h2"],
          "routes": [{
            "match": [{ "host": ["$SITE"] }],
            "handle": [{ "handler": "static_response", "body": "$BODY" }],
            "terminal": true
          }]
        }
      }
    },
    "tls": {
      "automation": { "policies": [{ "subjects": ["$SITE"], "issuers": [{ "module": "internal" }] }] }
    },
    "pki": { "certificate_authorities": { "local": { "install_trust": false } } }
  }
}
JSON
  echo "$file"
}
CA=$WORK/caddy-data/pki/authorities/local/root.crt
CADDY_PID=""
# Starts `$1` with the config `$2`. Waits for the admin API, 10 seconds at most.
start_caddy() {
  local program=$1 config=$2 log=$3 i
  "$program" run --config "$config" >"$log" 2>&1 &
  CADDY_PID=$!
  for i in $(seq 1 50); do
    if ! kill -0 "$CADDY_PID" 2>/dev/null; then
      wait "$CADDY_PID"
      echo "caddy exited with $?"
      CADDY_PID=""
      return 1
    fi
    curl -fsS -o /dev/null --max-time 1 http://127.0.0.1:18019/config/ 2>/dev/null && return 0
    sleep 0.2
  done
  echo "caddy did not answer on its admin port"
  return 1
}
stop_caddy() {
  [ -n "$CADDY_PID" ] || return 0
  kill -TERM "$CADDY_PID" 2>/dev/null
  wait "$CADDY_PID" 2>/dev/null
  CADDY_PID=""
}
bind_error() { grep -o 'listen tcp[^"]*' "$1" | head -1; }

# ------------------------------------------------------------- T3: setcap
section "T3 ports 80 and 443 with setcap"
LOW=$(caddy_config 80 443)
echo "--- T3a no capability"
run getcap "$CADDY"
start_caddy "$CADDY" "$LOW" "$OUT/caddy-t3a.log"
result T3a "caddy on 127.0.0.1:80/443, no capability" "$([ -n "$CADDY_PID" ] && echo STARTED || bind_error "$OUT/caddy-t3a.log")"
stop_caddy

echo "--- T3b cap_net_bind_service=+ep"
SETCAP=$(PATH=/usr/sbin:/sbin:/usr/bin command -v setcap)
GETCAP=$(PATH=/usr/sbin:/sbin:/usr/bin command -v getcap)
run sudo "$SETCAP" cap_net_bind_service=+ep "$CADDY"
run "$GETCAP" "$CADDY"
start_caddy "$CADDY" "$LOW" "$OUT/caddy-t3b.log"
result T3b "caddy on 127.0.0.1:80/443 after setcap" "$([ -n "$CADDY_PID" ] && echo "starts ($("$GETCAP" "$CADDY" | awk '{print $NF}'))" || bind_error "$OUT/caddy-t3b.log")"
if [ -n "$CADDY_PID" ]; then
  # With sudo: the same user cannot read the sockets of this Caddy (T4).
  run sudo ss -H -ltnp
  sockets=$(sudo ss -H -ltnp | grep "pid=$CADDY_PID," | awk '{print $4}' | sort | tr '\n' ' ')
  result T3b "caddy listening sockets" "$sockets"
  run curl -sk --max-time 5 --resolve "$SITE:443:127.0.0.1" "https://$SITE/"
  echo
  run curl -s -o /dev/null --max-time 5 -w '%{http_code} %{redirect_url}\n' --resolve "$SITE:80:127.0.0.1" "http://$SITE/"

  # -------------------------------------- T4: what the same user can see
  section "T4 a Caddy with a file capability, seen by the same user"
  run readlink "/proc/$CADDY_PID/exe"
  exe_rc=$?
  run ls -ld "/proc/$CADDY_PID"
  run stat -c '%U:%G' "/proc/$CADDY_PID"
  echo "\$ tr '\\0' ' ' </proc/$CADDY_PID/cmdline"
  tr '\0' ' ' <"/proc/$CADDY_PID/cmdline"
  cmdline_rc=$?
  echo
  run awk '{print "start time (field 22):", $22}' "/proc/$CADDY_PID/stat"
  stat_rc=$?
  run grep -E '^(Uid|CapPrm|CapEff|CapBnd)' "/proc/$CADDY_PID/status"
  run readlink "/proc/$CADDY_PID/cwd"
  run ps -o pid,user,comm,args -p "$CADDY_PID"
  result T4 "readlink /proc/<pid>/exe as the same user" "$([ "$exe_rc" = 0 ] && echo readable || echo "DENIED (exit $exe_rc)")"
  result T4 "/proc/<pid>/cmdline and stat" "cmdline exit $cmdline_rc, stat exit $stat_rc, owner $(stat -c '%U' "/proc/$CADDY_PID")"

  # ----------------------------------------- T8: the system trust store
  section "T8 the CA in the system trust store (whole chain: DNS, 443, trust)"
  run ls -l "$CA"
  run curl -sS --max-time 10 "https://$SITE/"
  result T8 "curl https://$SITE before trust" "exit $? (60 = unknown CA)"
  if [ "$FAMILY" = debian ]; then
    ANCHOR=/usr/local/share/ca-certificates/marsa-local-ca.crt
    UPDATE="/usr/sbin/update-ca-certificates"
  else
    ANCHOR=/etc/pki/ca-trust/source/anchors/marsa-local-ca.pem
    UPDATE="/usr/bin/update-ca-trust"
  fi
  run sudo install -m 0644 -o root -g root "$CA" "$ANCHOR"
  start=$(now_ms)
  run sudo "$UPDATE"
  update_rc=$?
  result T8 "$UPDATE" "exit $update_rc in $(($(now_ms) - start)) ms"
  run curl -sS --max-time 10 "https://$SITE/"
  curl_rc=$?
  echo
  body=$(curl -sS --max-time 10 "https://$SITE/" 2>&1)
  result T8 "curl https://$SITE with the system store" "exit $curl_rc, body: $body"
  run curl -s -o /dev/null --max-time 10 -w '%{http_code} %{redirect_url}\n' "http://$SITE/"
  result T8 "curl http://$SITE" "$(curl -s -o /dev/null --max-time 10 -w '%{http_code} -> %{redirect_url}' "http://$SITE/")"
  run python3 -c "import urllib.request; print(urllib.request.urlopen('https://$SITE/', timeout=10).read())"
  result T8 "python urllib (OpenSSL default store)" "exit $?"
  ls -l /etc/ssl/certs 2>/dev/null | grep -i marsa

  # --------------------------------------------------- T9: NSS (browsers)
  section "T9 NSS: Chrome and Firefox do not read the system store"
  CHROME=$(command -v google-chrome || command -v chromium || command -v chromium-browser || true)
  chrome_sees_site() {
    [ -n "$CHROME" ] || { echo "no chrome"; return 2; }
    timeout 90 "$CHROME" --headless=new --no-sandbox --disable-gpu --dump-dom "https://$SITE/" 2>"$OUT/chrome.err" | grep -q "$BODY"
  }
  if [ -n "$CHROME" ]; then
    run "$CHROME" --version
    chrome_sees_site
    result T9 "Chrome, CA in the system store only" "$([ $? = 0 ] && echo "page loads" || echo "page does NOT load")"
  else
    result T9 "Chrome" "not installed here"
  fi
  if ! have certutil; then
    echo "certutil is not installed; installing it for the test"
    start=$(now_ms)
    if [ "$FAMILY" = debian ]; then
      sudo apt-get install -y -q libnss3-tools >/dev/null 2>&1
    else
      sudo dnf install -y -q nss-tools >/dev/null 2>&1
    fi
    result T9 "certutil" "not installed by default; package installed in $(($(now_ms) - start)) ms"
  else
    result T9 "certutil" "already installed: $(command -v certutil)"
  fi
  NSSDB=$HOME/.pki/nssdb
  if [ ! -f "$NSSDB/cert9.db" ]; then
    mkdir -p "$NSSDB"
    run certutil -d "sql:$NSSDB" -N --empty-password
  fi
  run certutil -d "sql:$NSSDB" -A -t "C,," -n "Marsa Local CA" -i "$CA"
  result T9 "certutil -A into ~/.pki/nssdb, as the user" "exit $?"
  run certutil -d "sql:$NSSDB" -L
  if [ -n "$CHROME" ]; then
    chrome_sees_site
    result T9 "Chrome, CA also in ~/.pki/nssdb" "$([ $? = 0 ] && echo "page loads" || echo "page does NOT load")"
  fi
  run ls -d "$HOME"/.mozilla/firefox/*/ "$HOME"/snap/firefox/common/.mozilla/firefox/*/
  run certutil -d "sql:$NSSDB" -D -n "Marsa Local CA"
  result T9 "certutil -D (remove)" "exit $?"

  section "T8 removal from the system store"
  run sudo rm "$ANCHOR"
  if [ "$FAMILY" = debian ]; then
    run sudo "$UPDATE"
    curl -sS -o /dev/null --max-time 10 "https://$SITE/" 2>/dev/null
    result T8 "after rm + update-ca-certificates (no --fresh)" "curl exit $?"
    run sudo "$UPDATE" --fresh
  else
    run sudo "$UPDATE"
  fi
  curl -sS -o /dev/null --max-time 10 "https://$SITE/" 2>/dev/null
  result T8 "after removal" "curl exit $? (60 = unknown CA again)"

  section "T4 stop: SIGTERM from the same user"
  pid=$CADDY_PID
  run kill -TERM "$pid"
  wait "$pid" 2>/dev/null
  result T4 "kill -TERM from the same user" "caddy exited with $?"
  CADDY_PID=""
fi

section "T3c the capability after the file changes"
cp "$CADDY" "$WORK/caddy-copy"
run "$GETCAP" "$WORK/caddy-copy"
result T3c "a copy of the file (a new version is a new file)" "caps: $("$GETCAP" "$WORK/caddy-copy" | awk '{print $NF}' | grep . || echo none)"
cp "$CADDY" "$CADDY.tmp" && sudo "$SETCAP" cap_net_bind_service=+ep "$CADDY.tmp"
printf 'x' >>"$CADDY.tmp"
result T3c "the same file after a write" "caps: $("$GETCAP" "$CADDY.tmp" | awk '{print $NF}' | grep . || echo none)"
rm -f "$CADDY.tmp"

# -------------------------------------- T5: setcap without following links
section "T5 a root helper that does not follow a planted link"
cp "$WORK/caddy-copy" "$WORK/target-a"
cp "$WORK/caddy-copy" "$WORK/target-b"
ln -s "$WORK/target-b" "$WORK/link-to-b"
mkfifo "$WORK/fifo"
run sudo python3 "$ROOT/cap-probe.py" proc-fd "$WORK/target-a"
result T5 "setcap on /proc/<pid>/fd/<n> (O_PATH|O_NOFOLLOW)" "exit $?, caps: $("$GETCAP" "$WORK/target-a" | awk '{print $NF}' | grep . || echo none)"
run sudo "$SETCAP" -r "$WORK/target-a"
run sudo python3 "$ROOT/cap-probe.py" xattr "$WORK/target-a"
result T5 "fsetxattr on an O_NOFOLLOW descriptor" "exit $?, caps: $("$GETCAP" "$WORK/target-a" | awk '{print $NF}' | grep . || echo none)"
run sudo python3 "$ROOT/cap-probe.py" xattr "$WORK/link-to-b"
result T5 "the same through a symlink" "exit $?, target caps: $("$GETCAP" "$WORK/target-b" | awk '{print $NF}' | grep . || echo none)"
run sudo python3 "$ROOT/cap-probe.py" proc-fd "$WORK/link-to-b"
run sudo python3 "$ROOT/cap-probe.py" xattr "$WORK/fifo"
result T5 "a FIFO in place of the file" "exit $? (3 = refused, did not hang)"
run sudo "$SETCAP" cap_net_bind_service=+ep "$WORK/link-to-b"
result T5 "plain setcap on a symlink path" "exit $?, target caps: $("$GETCAP" "$WORK/target-b" | awk '{print $NF}' | grep . || echo none)"

# ------------------------------------------------------------- T6: nosuid
section "T6 a nosuid mount ignores file capabilities"
run sudo mkdir -p /mnt/marsa-nosuid
if run sudo mount -t tmpfs -o nosuid,size=128m tmpfs /mnt/marsa-nosuid; then
  sudo cp "$WORK/caddy-copy" /mnt/marsa-nosuid/caddy
  sudo chown "$(id -u):$(id -g)" /mnt/marsa-nosuid/caddy
  run sudo "$SETCAP" cap_net_bind_service=+ep /mnt/marsa-nosuid/caddy
  run "$GETCAP" /mnt/marsa-nosuid/caddy
  start_caddy /mnt/marsa-nosuid/caddy "$LOW" "$OUT/caddy-t6.log"
  result T6 "setcap'd caddy on a nosuid mount" "$([ -n "$CADDY_PID" ] && echo STARTED || bind_error "$OUT/caddy-t6.log")"
  stop_caddy
  run sudo umount /mnt/marsa-nosuid
else
  result T6 "nosuid mount" "could not mount a tmpfs here"
fi

# ------------------------------------------------------------- T7: sysctl
section "T7 the other way: net.ipv4.ip_unprivileged_port_start=80"
PLAIN=$WORK/caddy-copy
run "$GETCAP" "$PLAIN"
run sudo sysctl -w net.ipv4.ip_unprivileged_port_start=80
start_caddy "$PLAIN" "$LOW" "$OUT/caddy-t7.log"
result T7 "caddy without a capability on 80/443" "$([ -n "$CADDY_PID" ] && echo starts || bind_error "$OUT/caddy-t7.log")"
if [ -n "$CADDY_PID" ]; then
  run readlink "/proc/$CADDY_PID/exe"
  result T7 "readlink /proc/<pid>/exe as the same user" "$([ $? = 0 ] && echo readable || echo DENIED)"
  run curl -sk --max-time 5 --resolve "$SITE:443:127.0.0.1" "https://$SITE/"
  echo
fi
stop_caddy
bind_probe() { python3 -c "
import socket, sys
family = socket.AF_INET6 if ':' in sys.argv[1] else socket.AF_INET
s = socket.socket(family, socket.SOCK_STREAM)
try:
    s.bind((sys.argv[1], int(sys.argv[2])))
    print('bound')
except OSError as err:
    print('errno', err.errno, err.strerror)
" "$1" "$2"; }
result T7 "bind [::1]:80 and 127.0.0.1:79 as the user" "$(bind_probe ::1 80) / $(bind_probe 127.0.0.1 79)"
run sudo sysctl -w net.ipv4.ip_unprivileged_port_start=1024
result T7 "bind 127.0.0.1:80 after the reset to 1024" "$(bind_probe 127.0.0.1 80)"
echo 'net.ipv4.ip_unprivileged_port_start=80' | sudo tee /etc/sysctl.d/60-marsa.conf
run sudo sysctl -p /etc/sysctl.d/60-marsa.conf
result T7 "sysctl -p <our file>" "exit $?, value $(cat /proc/sys/net/ipv4/ip_unprivileged_port_start)"
run sudo rm /etc/sysctl.d/60-marsa.conf
run sudo sysctl -w net.ipv4.ip_unprivileged_port_start=1024

# ------------------------------------------------------------- T10: pkexec
section "T10 pkexec with no screen"
if ! have pkexec; then
  echo "pkexec is not installed; installing it for the test"
  if [ "$FAMILY" = debian ]; then
    sudo apt-get install -y -q pkexec polkitd >/dev/null 2>&1 || sudo apt-get install -y -q policykit-1 >/dev/null 2>&1
  else
    sudo dnf install -y -q polkit >/dev/null 2>&1
  fi
  result T10 "pkexec" "not installed by default here; installed for the test"
fi
PKEXEC=$(command -v pkexec)
run "$PKEXEC" --version
POLKIT=$("$PKEXEC" --version | awk '{print $NF}')
run systemctl is-active polkit
echo "--- no rule"
run timeout 30 "$PKEXEC" /usr/bin/id -u </dev/null
result T10 "pkexec without a rule, no agent, no terminal" "exit $? (124 = it waited 30 s)"
ME=$(id -un)
if [ -d /etc/polkit-1/localauthority/50-local.d ] && [ "${POLKIT%%.*}" = 0 ]; then
  RULE=/etc/polkit-1/localauthority/50-local.d/marsa-ci.pkla
  printf '[Marsa CI: %s may use pkexec]\nIdentity=unix-user:%s\nAction=org.freedesktop.policykit.exec\nResultAny=yes\nResultInactive=yes\nResultActive=yes\n' "$ME" "$ME" | sudo tee "$RULE"
else
  RULE=/etc/polkit-1/rules.d/49-marsa-ci.rules
  sudo mkdir -p /etc/polkit-1/rules.d
  printf 'polkit.addRule(function(action, subject) {\n  if (action.id == "org.freedesktop.policykit.exec" && subject.user == "%s") {\n    return polkit.Result.YES;\n  }\n});\n' "$ME" | sudo tee "$RULE"
fi
sleep 2
echo "--- with the rule $RULE"
run "$PKEXEC" /usr/bin/id -u </dev/null
result T10 "pkexec with a rule for this user (polkit $POLKIT)" "exit $?, runs as uid $("$PKEXEC" /usr/bin/id -u </dev/null 2>/dev/null)"
run "$PKEXEC" /usr/bin/env </dev/null
run "$PKEXEC" /bin/pwd </dev/null
result T10 "environment under pkexec" "PKEXEC_UID=$("$PKEXEC" /usr/bin/env </dev/null | sed -n 's/^PKEXEC_UID=//p'), cwd $("$PKEXEC" /bin/pwd </dev/null), PATH=$("$PKEXEC" /usr/bin/env </dev/null | sed -n 's/^PATH=//p')"
REQUEST='{"version":1,"ops":[{"op":"trust_install","pem":"-----BEGIN CERTIFICATE-----\nMIIB a '"'"'b'"'"' $HOME `id`\n"}]}'
got=$("$PKEXEC" /usr/bin/printf '%s' "$REQUEST" </dev/null)
result T10 "a JSON argument with quotes, \$ and backticks" "$([ "$got" = "$REQUEST" ] && echo "arrives unchanged" || echo "CHANGED: $got")"
BIG=$(head -c 65536 /dev/zero | tr '\0' 'a')
result T10 "one argument of 64 KiB" "$("$PKEXEC" /usr/bin/python3 -c 'import sys; print(len(sys.argv[1]), "bytes arrive")' "$BIG" </dev/null 2>&1 | tail -1)"
"$PKEXEC" /usr/bin/python3 -c 'import sys; print("to stdout"); print("to stderr", file=sys.stderr); sys.exit(1)' </dev/null >"$WORK/pk.out" 2>"$WORK/pk.err"
rc1=$?
"$PKEXEC" /usr/bin/python3 -c 'import sys; sys.exit(2)' </dev/null
rc2=$?
result T10 "exit codes 1 and 2 of the program" "pkexec exits $rc1 and $rc2; stdout '$(cat "$WORK/pk.out")', stderr '$(cat "$WORK/pk.err")'"
cp /usr/bin/id "$HOME/id-copy"
run "$PKEXEC" "$HOME/id-copy" -u </dev/null
result T10 "a program in the user's home folder" "exit $?"
rm -f "$HOME/id-copy"
run "$PKEXEC" /nonexistent/marsa-helper </dev/null
result T10 "a program that does not exist" "exit $?"
run setsid "$PKEXEC" /usr/bin/id -u </dev/null
result T10 "from a process with no terminal (setsid)" "exit $?"
run sudo rm "$RULE"
sleep 2
timeout 30 "$PKEXEC" /usr/bin/id -u </dev/null >/dev/null 2>&1
result T10 "after the rule is removed" "exit $?"

# ------------------------------------------------------- T2 again: removal
section "T2 removal of the drop-in"
run sudo rm "$DROPIN"
restart_resolved
run resolvectl query "$SITE"
result T2 "$SITE after the drop-in is removed" "$(resolvectl query "$SITE" >/dev/null 2>&1 && echo "STILL resolves" || echo "does not resolve")"
AFTER=$(time_other_names)
echo "$AFTER"
result T2 "5 fresh lookups outside .test, after removal" "$(echo "$AFTER" | total_ms) ms total"
stop_dns

rm -rf "$WORK"
section "summary"
cat "$SUMMARY"
