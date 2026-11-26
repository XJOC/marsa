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
# The libraries that Oracle's archive does not carry on Linux, as
# .github/workflows/libaio-build.yml builds them (versions as in recipes/mysql.toml).
LIBAIO_VERSION=0.3.113
NUMACTL_VERSION=2.0.19
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) NAME=mysql-$VERSION-macos15-arm64.tar.gz ;;
  Linux-x86_64) NAME=mysql-$VERSION-linux-glibc2.28-x86_64.tar.xz ;;
  Linux-aarch64) NAME=mysql-$VERSION-linux-glibc2.28-aarch64.tar.xz ;;
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
# xz-utils is on a runner; Marsa itself uses /usr/bin/xz the same way.
case "$NAME" in
  *.tar.xz) tar -xJf "$RUNNER_TEMP/$NAME" -C "$MY" --strip-components 1 ;;
  *) tar -xzf "$RUNNER_TEMP/$NAME" -C "$MY" --strip-components 1 ;;
esac
rm -f "$RUNNER_TEMP/$NAME"

if [ "$(uname -s)" = Linux ]; then
  step "libaio.so.1 and libnuma.so.1: built in almalinux:8 as libaio-build does, saved in lib/private"
  mkdir -p "$RUNNER_TEMP/libs"
  run docker run --rm \
    --volume "$REPO/tools/libaio-build:/work/libaio-build:ro" \
    --volume "$RUNNER_TEMP/libs:/out" \
    --env LIBAIO_VERSION="$LIBAIO_VERSION" --env NUMACTL_VERSION="$NUMACTL_VERSION" \
    almalinux:8 \
    bash -c 'dnf install -y -q gcc make binutils file tar gzip autoconf automake libtool && bash /work/libaio-build/build.sh /out'
  cp "$RUNNER_TEMP/libs/libaio.so.1" "$RUNNER_TEMP/libs/libnuma.so.1" "$MY/lib/private/"
  # Every library mysqld needs is found, and libaio and libnuma are ours, not
  # the system's (a RUNPATH comes before the system's folders).
  ldd "$MY/bin/mysqld" | tee "$RUNNER_TEMP/ldd.txt"
  ! grep -q 'not found' "$RUNNER_TEMP/ldd.txt" || fail "mysqld has a library that it cannot find"
  for lib in libaio.so.1 libnuma.so.1; do
    grep -E "^[[:space:]]*$lib => $MY/bin/\.\./lib/private/$lib " "$RUNNER_TEMP/ldd.txt" ||
      fail "$lib is not the one in lib/private"
  done
fi
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
grep -qx "DB_PORT=$PORT" "$RUNNER_TEMP/services.txt" || fail "no DB_PORT line"
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

step "the database browser: app and its tables, none of the server's own, and a close that closes"
# shellcheck source=api.sh
source "$(dirname "$0")/api.sh"
sources=$(api_ok GET /v1/db/sources)
[ "$(jq -r '.servers[] | select(.server == "mysql") | .state' <<<"$sources")" = running ] || fail "sources: $sources"
names=$(jq -c '[.servers[] | select(.server == "mysql") | .databases[].name]' <<<"$sources")
echo "databases: $names"
jq -e 'index("app") != null and all(.[]; IN("mysql", "sys", "performance_schema", "information_schema") | not)' <<<"$names" >/dev/null ||
  fail "databases: $names"
tables=$(api_ok POST /v1/db/tables '{"source": {"kind": "server", "server": "mysql", "database": "app"}}')
[ "$(jq -c '[.tables[] | [.table.name, .kind]]' <<<"$tables")" = '[["migrations","table"],["posts","table"]]' ] || fail "tables: $tables"
[ "$(api_status POST /v1/db/tables '{"source": {"kind": "server", "server": "mysql", "database": "mysql"}}')" = "404 db_not_found" ] ||
  fail "a system database was opened"
# Columns, a page, and one whole value, read as text through the API.
table=$(api_ok POST /v1/db/table '{"source": {"kind": "server", "server": "mysql", "database": "app"}, "table": {"name": "posts"}}')
[ "$(jq -c '[.columns[] | [.name, .key]]' <<<"$table")" = '[["id","primary"],["body",null]]' ] || fail "columns: $table"
page=$(api_ok POST /v1/db/rows '{"source": {"kind": "server", "server": "mysql", "database": "app"}, "table": {"name": "posts"}, "offset": 0, "count": true}')
[ "$(jq -c '.rows' <<<"$page")" = '[["1","مرحبا بالعالم"],["2","hello world"]]' ] || fail "rows: $page"
[ "$(jq -c '.total' <<<"$page")" = '{"rows":2,"exact":true}' ] || fail "total: $page"
value=$(api_ok POST /v1/db/cell '{"source": {"kind": "server", "server": "mysql", "database": "app"}, "table": {"name": "posts"}, "column": "body", "key": ["1"]}')
[ "$(jq -r '.cell' <<<"$value")" = "مرحبا بالعالم" ] || fail "cell: $value"
missing=$(api_status POST /v1/db/cell '{"source": {"kind": "server", "server": "mysql", "database": "app"}, "table": {"name": "posts"}, "column": "body", "key": ["9"]}')
[ "$missing" = "404 db_row_not_found" ] || fail "a missing row: $missing"
# One connection for each source (the server's catalog, and app), no pool,
# both over TCP: the socket's root would show `localhost` as its host.
others="select count(*) from information_schema.processlist where user = 'root' and host like '127.0.0.1:%' and id <> connection_id()"
[ "$(tcp -e "$others")" = 2 ] || fail "$(tcp -e "$others") browser connections, not 2"
api_ok POST /v1/db/close >/dev/null
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ "$(tcp -e "$others")" = 0 ] && break
  sleep 0.5
done
[ "$(tcp -e "$others")" = 0 ] || fail "a browser connection stayed open after close"
# The user's SQL: a job whose rows are read once, a write the engine
# refuses, and a cancel that stops the statement inside the server.
app='{"kind": "server", "server": "mysql", "database": "app"}'
id=$(api_query "$app" "SELECT id, body FROM posts ORDER BY id")
[ "$(api_job_end "$id" | jq -r .status)" = done ] || fail "query $id"
result=$(api_ok GET "/v1/db/results/$id")
[ "$(jq -c .rows <<<"$result")" = '[["1","مرحبا بالعالم"],["2","hello world"]]' ] || fail "result: $result"
id=$(api_query "$app" "INSERT INTO posts (body) VALUES ('x')")
end=$(api_job_end "$id")
[ "$(jq -r .code <<<"$end")" = db_read_only ] || fail "a write: $end"
[ "$(tcp app -e 'select count(*) from posts')" = 2 ] || fail "the write went through"
id=$(api_query "$app" "SELECT SLEEP(30)")
sleep 1
api_ok DELETE "/v1/jobs/$id" >/dev/null
[ "$(api_job_end "$id" | jq -r .code)" = db_query_cancelled ] || fail "the cancel"
sleeping="select count(*) from information_schema.processlist where info like 'SELECT SLEEP%'"
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ "$(tcp -e "$sleeping")" = 0 ] && break
  sleep 0.3
done
[ "$(tcp -e "$sleeping")" = 0 ] || fail "the cancelled statement still runs in MySQL"
# Closes what the query step opened, so the next sign-in is a new one.
api_ok POST /v1/db/close >/dev/null
# root over TCP with a password: the browser says so, and asks nothing else.
sock -e "alter user 'root'@'127.0.0.1' identified by 'e2e-secret'"
problem=$(api_ok GET /v1/db/sources | jq -r '.servers[] | select(.server == "mysql") | .problem')
sock -e "alter user 'root'@'127.0.0.1' identified by ''"
[ "$problem" = db_sign_in_failed ] || fail "with a password for root: $problem"
api_ok POST /v1/db/close >/dev/null

step "marsa db: the browser on the command line, 100,000 rows, read-only, a cancel, and the idle close"
ms() { perl -MTime::HiRes=time -e 'printf "%d", time * 1000'; }
marsa db | tee "$RUNNER_TEMP/db.txt"
grep -q '^mysql/app ' "$RUNNER_TEMP/db.txt" || fail "marsa db lists no mysql/app"
! grep -qE '^mysql/(mysql|sys|performance_schema|information_schema) ' "$RUNNER_TEMP/db.txt" || fail "a system database is listed"
tcp app -e "set session cte_max_recursion_depth = 200000; create table big (id int primary key, v varchar(20));
  insert into big with recursive n(i) as (select 1 union all select i + 1 from n where i < 100000) select i, concat('row ', i) from n"
marsa db tables mysql/app | grep -q '^big ' || fail "marsa db tables: no big"
marsa db rows mysql/app big | tail -n 1 | grep -qx 'Rows 1 to 500 of 100,000.' || fail "the first page"
begun=$(ms)
last=$(marsa db rows mysql/app big --page 200 | tail -n 1)
echo "the last page (OFFSET 99,500) took $(($(ms) - begun)) ms"
[ "$last" = 'Rows 99,501 to 100,000 of 100,000.' ] || fail "the last page: $last"
[ "$(marsa db rows mysql/app big --sort id --desc | sed -n 2p | awk '{print $1}')" = 100000 ] || fail "the reverse page"
for sql in "INSERT INTO big VALUES (100001, 'x')" "CREATE TABLE t2 (a int)" "DROP TABLE big"; do
  if out=$(marsa db query mysql/app "$sql" 2>&1); then fail "a write ran: $sql"; fi
  grep -q 'db_read_only' <<<"$out" || fail "$sql: $out"
done
[ "$(tcp app -e 'select count(*) from big')" = 100000 ] || fail "the rows changed"
[ "$(tcp app -e 'select count(*) from information_schema.tables where table_schema = "app"')" = 3 ] || fail "the tables changed"
begun=$(ms)
if out=$(marsa db query --timeout 2 mysql/app "SELECT SLEEP(60)" 2>&1); then fail "the sleep was not cancelled"; fi
took=$(($(ms) - begun))
echo "the cancel took $took ms"
grep -q db_query_cancelled <<<"$out" || fail "--timeout: $out"
[ "$took" -lt 5000 ] || fail "--timeout 2 took $took ms"
[ "$(tcp -e "$sleeping")" = 0 ] || fail "the cancelled statement still runs in MySQL"
if out=$(marsa db tables mysql/mysql 2>&1); then fail "a system database was opened"; fi
grep -q db_not_found <<<"$out" || fail "mysql/mysql: $out"
echo "no request for 70 s: the browser's connections close by themselves"
sleep 70
[ "$(tcp -e "$others")" = 0 ] || fail "a browser connection stayed open after 70 s idle"
# Open again, for the stop below: it must not wait for the browser.
marsa db tables mysql/app >/dev/null

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
# SO_REUSEADDR, as every server sets: the sockets that the steps before left in
# TIME_WAIT on this port would make a bind without it fail.
python3 -c "import socket, time; s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', $PORT)); s.listen(); time.sleep(300)" &
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
started=false
for _ in $(seq 1 60); do
  if marsa services | grep -qE '^mysql +running'; then
    started=true
    break
  fi
  sleep 1
done
[ "$started" = true ] || fail "mysql did not start with marsad"
# No retry: `running` means the server takes connections.
tcp -e 'select 1' >/dev/null || fail "mysql is running and does not answer"
[ "$(tcp app -e 'select count(*) from posts')" = 2 ] || fail "the data did not survive"

step "marsad killed: the next start stops our old mysqld with SIGTERM"
OLD=$(cat "$DATA/run/mysqld.pid")
pkill -9 -f "marsad.*--data-dir $DATA"
sleep 1
kill -0 "$OLD" || fail "mysqld died with marsad"
run marsa services start mysql
kill -0 "$OLD" 2>/dev/null && fail "the orphan still runs"
marsa logs mysql | grep -F "that an earlier marsad left running; it ended after SIGTERM" ||
  fail "no line about the orphan"
run marsa services stop mysql

step "PASS"
