#!/usr/bin/env bash
# End to end: MySQL 8.4 as a service of marsad, with a real server (task
# M6-05; ADR 0023). On a GitHub runner.
#
# MySQL is not in the signed manifest yet, so this downloads the Oracle
# archive that `recipes/mysql.toml` names, checks it against its `.md5`, and
# unpacks it where `marsa install mysql` would. Every marsa command runs
# with --data-dir in a new folder; no sudo.
#
# Before it: `cargo build --release -p marsa`.
set -euo pipefail

if [ "${CI:-}" != "true" ]; then
  echo "This script downloads MySQL and PHP. It runs only with CI=true (a runner)." >&2
  exit 2
fi

REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$REPO/target/release
VERSION=8.4.13
RUNNER_TEMP=${RUNNER_TEMP:?the runner sets RUNNER_TEMP}
# Short, so the socket path `<data>/run/mysql.sock` stays under the limit of
# a Unix socket (about 103 bytes on macOS).
DATA=$RUNNER_TEMP/my-e2e
# Another port only for a run on a machine whose 3306 is taken.
PORT=${MYSQL_E2E_PORT:-3306}
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) NAME=mysql-$VERSION-macos15-arm64.tar.gz ;;
  *) echo "no MySQL build in this script for $(uname -sm)" >&2; exit 2 ;;
esac
MY=$DATA/runtimes/mysql/$VERSION

step() { printf '\n\n===== %s =====\n' "$*"; }
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
run() {
  printf '$ %s\n' "$*"
  "$@"
}
marsa() { "$BIN/marsa" --data-dir "$DATA" "$@"; }
# The mysql client over TCP (root@127.0.0.1) or the socket (root@localhost),
# with no option file and no password.
tcp() { "$MY/bin/mysql" --no-defaults -uroot -h 127.0.0.1 -P "$PORT" --protocol=tcp -N -B "$@"; }
sock() { "$MY/bin/mysql" --no-defaults -uroot -S "$DATA/run/mysql.sock" -N -B "$@"; }
# Fails unless `cmd` fails and its output has `text`.
refused() {
  local text=$1
  shift
  local out
  if out=$("$@" 2>&1); then
    fail "accepted, but must be refused: $* -> $out"
  fi
  printf '%s\n' "$out"
  grep -qF -- "$text" <<<"$out" || fail "refused, but without \"$text\": $*"
}
# Every TCP listener of our mysqld.
listeners() {
  local pid
  pid=$(cat "$DATA/run/mysqld.pid")
  if [ "$(uname -s)" = Linux ]; then
    ss -Hltnp | grep -F "pid=$pid," | awk '{print $4}'
  else
    lsof -nP -a -p "$pid" -iTCP -sTCP:LISTEN -Fn | sed -n 's/^n//p'
  fi
}
rss_mb() { echo $(($(ps -o rss= -p "$1") / 1024)); }

# What helps to understand a failure. Never `run/daemon.json`: it has the token.
diagnose() {
  step "diagnosis after a failure"
  set +e
  marsa services
  marsa logs mysql | tail -n 60
  tail -n 60 "$DATA/logs/marsad.log"
  listeners
}
trap 'code=$?; [ "$code" -eq 0 ] || diagnose; marsa daemon stop >/dev/null 2>&1; exit "$code"' EXIT

step "MySQL $VERSION ($NAME), checked against its .md5"
rm -rf "$DATA"
mkdir -p "$MY" "$DATA/config"
URL=https://cdn.mysql.com/Downloads/MySQL-8.4/$NAME
run curl -fsSL -o "$RUNNER_TEMP/$NAME" "$URL"
run curl -fsSL -o "$RUNNER_TEMP/$NAME.md5" "$URL.md5"
want=$(awk '{print $1}' "$RUNNER_TEMP/$NAME.md5")
got=$(openssl md5 -r "$RUNNER_TEMP/$NAME" | awk '{print $1}')
[ "$want" = "$got" ] || fail "md5 $got, expected $want"
tar -xzf "$RUNNER_TEMP/$NAME" -C "$MY" --strip-components 1
"$MY/bin/mysqld" --version | tee "$RUNNER_TEMP/version.txt"
grep -qF " Ver $VERSION " "$RUNNER_TEMP/version.txt" || fail "mysqld --version"
printf '[mysql]\ndefault = "8.4"\nport = %s\n' "$PORT" >"$DATA/config/global.toml"

step "marsa services start mysql: the first init, then the server"
begun=$(date +%s)
run marsa services start mysql
echo "the first start took $(($(date +%s) - begun)) s"
marsa services | tee "$RUNNER_TEMP/services.txt"
grep -E '^mysql +running' "$RUNNER_TEMP/services.txt" || fail "mysql is not running"
grep -qF "mysql: 127.0.0.1:$PORT, user root, no password" "$RUNNER_TEMP/services.txt" ||
  fail "no connection line"
grep -q 'enabled = true' "$DATA/config/global.toml" || fail "the start did not turn it on"
[ -d "$DATA/databases/mysql/8.4/mysql" ] || fail "no data folder in databases/mysql/8.4"

step "it listens on 127.0.0.1:$PORT only: no other address, no X protocol"
[ -n "$(listeners)" ] || fail "no listener"
listeners | tee "$RUNNER_TEMP/listeners.txt"
[ "$(cat "$RUNNER_TEMP/listeners.txt")" = "127.0.0.1:$PORT" ] || fail "listeners"

step "memory when idle: under 250 MB"
PID=$(cat "$DATA/run/mysqld.pid")
# The first seconds after the start hold the pages of InnoDB's start; macOS
# gives them back within a few seconds (254 MB, then 91 MB), a runner a bit
# later. So the limit is checked until it holds, for at most a minute.
sleep 10
for _ in $(seq 1 10); do
  echo "mysqld rss: $(rss_mb "$PID") MB"
  [ "$(rss_mb "$PID")" -lt 250 ] && break
  sleep 5
done
[ "$(rss_mb "$PID")" -lt 250 ] || fail "too much memory"

step "access (ADR 0023): root over TCP has no administrative privilege"
[ "$(tcp -e 'select 1')" = 1 ] || fail "root over TCP"
[ "$(tcp -e 'select current_user()')" = root@127.0.0.1 ] || fail "TCP is not root@127.0.0.1"
refused 'FILE privilege' tcp -e "select 1 into outfile '$RUNNER_TEMP/pwn'"
refused 'FILE privilege' tcp -e "select 1 into dumpfile '$RUNNER_TEMP/pwn'"
refused 'SHUTDOWN privilege' tcp -e 'shutdown'
refused 'CREATE USER privilege' tcp -e "create user evil@'%'"
# The grant tables are out of reach: a change there would give FILE and
# SUPER at the next start.
refused 'UPDATE command denied' tcp -e "update mysql.user set File_priv = 'Y' where user = 'root'"
refused 'INSERT command denied' tcp -e "insert into mysql.db (Host, Db, User) values ('%', 'x', 'evil')"
refused 'SELECT command denied' tcp -e 'select count(*) from mysql.user'
refused 'Access denied' tcp -e "grant all on *.* to 'root'@'127.0.0.1'"
refused 'SUPER' tcp -e 'set global general_log = 1'
[ ! -e "$RUNNER_TEMP/pwn" ] || fail "root wrote a file"
[ "$(sock -e 'select current_user()')" = root@localhost ] || fail "the socket is not root@localhost"
# secure_file_priv: even root@localhost writes only in databases/mysql/files.
refused 'secure-file-priv' sock -e "select 1 into outfile '$RUNNER_TEMP/pwn'"
sock -e "select 1 into outfile '$DATA/databases/mysql/files/ok.txt'"
[ -f "$DATA/databases/mysql/files/ok.txt" ] || fail "no file in secure_file_priv"
[ "$(ls -ld "$DATA/run" | cut -c1-10)" = drwx------ ] || fail "run/ is not 0700"

step "a Laravel-like app database, with utf8mb4"
tcp -e 'create database app'
# A routine that would run as root@localhost.
refused 'SET_ANY_DEFINER' tcp app -e "create definer = 'root'@'localhost' procedure p() select 1"
tcp app -e 'create table posts (id bigint unsigned auto_increment primary key, body text not null)'
tcp app -e "insert into posts (body) values ('مرحبا بالعالم'), ('hello world')"
[ "$(tcp app -e "select count(*) from posts where body like '%بالعالم%'")" = 1 ] || fail "Arabic text"

step "PHP's pdo_mysql as root with no password, as Laravel's default .env"
run marsa install php 8.4
PHP=$(ls -d "$DATA"/runtimes/php/8.4.*/bin/php | tail -n 1)
"$PHP" -r '
$db = new PDO("mysql:host=127.0.0.1;port='"$PORT"';dbname=app", "root", "");
$db->exec("create table migrations (id int unsigned auto_increment primary key, migration varchar(255) not null)");
$db->exec("insert into migrations (migration) values (\x27create_users_table\x27)");
echo $db->query("select count(*) from migrations")->fetchColumn(), "\n";
' | grep -qx 1 || fail "pdo_mysql"

step "the running version cannot be uninstalled; its databases stay"
refused 'in use' marsa uninstall mysql "$VERSION"
[ -d "$MY" ] || fail "the running version was removed"

step "a stop with an idle client: SIGTERM, done in under 10 s"
tcp app -e 'select sleep(120)' >/dev/null 2>&1 &
IDLE=$!
sleep 1
begun=$(date +%s)
run marsa services stop mysql
took=$(($(date +%s) - begun))
echo "the stop took $took s"
[ "$took" -lt 10 ] || fail "the stop took $took s"
wait "$IDLE" || true
grep -q 'Shutdown complete' "$DATA/logs/mysql.log" || fail "no 'Shutdown complete' in the log"
grep -q 'enabled = true' "$DATA/config/global.toml" && fail "the stop did not turn it off"

step "our mysqld runs on the data folder, outside marsad: refused at once"
"$MY/bin/mysqld" --defaults-file="$DATA/run/mysql.cnf" >"$RUNNER_TEMP/outside.log" 2>&1 &
OUTSIDE=$!
for _ in $(seq 1 60); do
  [ -s "$DATA/run/mysqld.pid" ] && tcp -e 'select 1' >/dev/null 2>&1 && break
  sleep 0.5
done
begun=$(date +%s)
refused 'service_data_dir_busy' marsa services start mysql
took=$(($(date +%s) - begun))
[ "$took" -lt 3 ] || fail "the refusal took $took s"
kill -0 "$OUTSIDE" || fail "the other mysqld was stopped"
kill -TERM "$OUTSIDE"
wait "$OUTSIDE" || true

step "another program holds the port: refused, with its name"
python3 -c "import socket, time; s = socket.socket(); s.bind(('127.0.0.1', $PORT)); s.listen(); time.sleep(300)" &
HOLDER=$!
sleep 1
refused 'service_port_busy' marsa services start mysql
kill "$HOLDER"
wait "$HOLDER" || true

step "a second start makes no new data folder, and it starts with marsad when on"
run marsa services start mysql
[ "$(grep -c 'making the data folder.*server="mysql"' "$DATA/logs/marsad.log")" = 1 ] ||
  fail "the init ran twice"
run marsa daemon stop
run marsa daemon start
# `running` comes with the process; its first answer may come later.
started=false
for _ in $(seq 1 60); do
  if marsa services | grep -qE '^mysql +running' && tcp -e 'select 1' >/dev/null 2>&1; then
    started=true
    break
  fi
  sleep 1
done
[ "$started" = true ] || fail "mysql did not start with marsad"
[ "$(tcp app -e 'select count(*) from posts')" = 2 ] || fail "the data did not survive"

step "marsad killed: the next start stops our old mysqld with SIGTERM"
OLD=$(cat "$DATA/run/mysqld.pid")
pkill -9 -f "marsad.*--data-dir $DATA" || pkill -9 -x marsad
sleep 1
kill -0 "$OLD" || fail "mysqld died with marsad"
run marsa services start mysql
kill -0 "$OLD" 2>/dev/null && fail "the orphan still runs"
marsa logs mysql | grep -F "that an earlier marsad left running; it ended after SIGTERM" ||
  fail "no line about the orphan"
run marsa services stop mysql

step "PASS"
