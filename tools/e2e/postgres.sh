#!/usr/bin/env bash
# End to end: PostgreSQL as a service of marsad, with a real server (task
# M6-03, part C3; ADR 0023). On a GitHub runner, Linux or macOS.
#
# PostgreSQL is not in the signed manifest yet, so this downloads the
# theseus-rs archive that `recipes/postgres.toml` names, checks it against
# its `.sha256`, and unpacks it where `marsa install postgres` would. Every
# marsa command runs with --data-dir in a new folder; no sudo.
#
# Before it: `cargo build --release -p marsa`.
set -euo pipefail

if [ "${CI:-}" != "true" ]; then
  echo "This script downloads PostgreSQL and PHP. It runs only with CI=true (a runner)." >&2
  exit 2
fi

REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$REPO/target/release
VERSION=18.7.0
# Short, so the socket path `<data>/run/.s.PGSQL.5432` stays under the
# limit of a Unix socket (about 103 bytes on macOS).
RUNNER_TEMP=${RUNNER_TEMP:?the runner sets RUNNER_TEMP}
DATA=$RUNNER_TEMP/pg-e2e
PORT=5432
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  *) echo "no PostgreSQL build for $(uname -sm)" >&2; exit 2 ;;
esac
PG=$DATA/runtimes/postgres/$VERSION
PSQL=$PG/bin/psql

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
# psql over TCP or the socket, never asking for a password.
sql() { "$PSQL" -X -w -v ON_ERROR_STOP=1 -At "$@"; }
tcp() { sql -h 127.0.0.1 -p "$PORT" "$@"; }
sock() { sql -h "$DATA/run" -p "$PORT" "$@"; }
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
listeners() {
  if [ "$(uname -s)" = Linux ]; then
    ss -Hltn "sport = :$PORT" | awk '{print $4}'
  else
    lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -Fn | sed -n 's/^n//p'
  fi
}

# What helps to understand a failure. Never `run/daemon.json`: it has the token.
diagnose() {
  step "diagnosis after a failure"
  set +e
  marsa services
  marsa logs postgres | tail -n 60
  tail -n 60 "$DATA/logs/marsad.log"
  listeners
}
trap 'code=$?; [ "$code" -eq 0 ] || diagnose; marsa daemon stop >/dev/null 2>&1; exit "$code"' EXIT

step "PostgreSQL $VERSION for $TARGET, checked against its .sha256"
rm -rf "$DATA"
mkdir -p "$PG" "$DATA/config"
NAME=postgresql-$VERSION-$TARGET.tar.gz
URL=https://github.com/theseus-rs/postgresql-binaries/releases/download/$VERSION/$NAME
run curl -fsSL -o "$RUNNER_TEMP/$NAME" "$URL"
run curl -fsSL -o "$RUNNER_TEMP/$NAME.sha256" "$URL.sha256"
(cd "$RUNNER_TEMP" && shasum -a 256 -c "$NAME.sha256")
tar -xzf "$RUNNER_TEMP/$NAME" -C "$PG" --strip-components 1
"$PG/bin/postgres" --version
printf '[postgres]\ndefault = "18"\n' >"$DATA/config/global.toml"

step "marsa services start postgres: the first init, then the server"
run marsa services start postgres
marsa services | tee "$RUNNER_TEMP/services.txt"
grep -E '^postgres +running' "$RUNNER_TEMP/services.txt" || fail "postgres is not running"
grep -qF "postgres: 127.0.0.1:$PORT, user root, no password" "$RUNNER_TEMP/services.txt" ||
  fail "no connection line"
grep -qx "DB_PORT=$PORT" "$RUNNER_TEMP/services.txt" || fail "no DB_PORT line"
grep -q 'enabled = true' "$DATA/config/global.toml" || fail "the start did not turn it on"
[ -f "$DATA/databases/postgres/18/PG_VERSION" ] || fail "no cluster in databases/postgres/18"

step "it listens on 127.0.0.1:$PORT only: nothing on ::1 or any address"
listeners | tee "$RUNNER_TEMP/listeners.txt"
[ -s "$RUNNER_TEMP/listeners.txt" ] || fail "nothing listens on $PORT"
if grep -v -E "^127\.0\.0\.1:$PORT$" "$RUNNER_TEMP/listeners.txt"; then
  fail "a listener on another address"
fi

step "access (ADR 0023, option B): root over TCP, postgres over the socket only"
[ "$(tcp -U root -d postgres -c 'select 1')" = 1 ] || fail "root over TCP"
[ "$(tcp -U root -d postgres -c 'select rolsuper::text || rolcreatedb::text from pg_roles where rolname = current_user')" = falsetrue ] ||
  fail "root must have CREATEDB and not be a superuser"
refused 'pg_hba.conf rejects connection' tcp -U postgres -d postgres -c 'select 1'
refused 'pg_hba.conf rejects connection' tcp -U nobody -d postgres -c 'select 1'
[ "$(sock -U postgres -d postgres -c 'select rolsuper from pg_roles where rolname = current_user')" = t ] ||
  fail "postgres over the socket"
refused 'pg_hba.conf rejects connection' sock -U root -d postgres -c 'select 1'
# Physical replication: refused by pg_hba.conf. Logical: `all` matches it,
# and root has no REPLICATION.
refused 'replication connection' sql -d "host=127.0.0.1 port=$PORT user=root dbname=postgres replication=true" -c 'IDENTIFY_SYSTEM'
refused 'WAL sender' sql -d "host=127.0.0.1 port=$PORT user=root dbname=postgres replication=database" -c 'IDENTIFY_SYSTEM'
# Mode 0700: only the owner may open run/ and use the socket.
[ "$(ls -ld "$DATA/run" | cut -c1-10)" = drwx------ ] || fail "run/ is not 0700"
ls -la "$DATA/run" | grep -E "^srwx------ .* \.s\.PGSQL\.$PORT\$" || fail "the socket is not 0700"

step "root cannot become more than it is"
[ "$(tcp -U root -d postgres -c "select string_agg(rolname, ',' order by rolname) from pg_roles where rolsuper or rolcreaterole or rolreplication or rolbypassrls")" = postgres ] ||
  fail "a privileged role other than postgres"
[ "$(tcp -U root -d postgres -c "select count(*) from pg_auth_members where member = 'root'::regrole")" = 0 ] ||
  fail "root is a member of a role"
refused 'permission denied to grant role' tcp -U root -d postgres -c 'grant pg_execute_server_program to root'
refused 'permission denied to create role' tcp -U root -d postgres -c 'create role evil superuser login'
refused 'permission denied to alter role' tcp -U root -d postgres -c 'alter role root superuser'
refused 'permission denied to set role' tcp -U root -d postgres -c 'set role postgres'
refused 'permission denied to set parameter' tcp -U root -d postgres -c "alter system set archive_command = 'id'"

step "root cannot run programs, or read or write files, as the owner"
refused 'permission denied to COPY to or from an external program' tcp -U root -d postgres -c "copy (select 1) to program 'id'"
refused 'permission denied for function pg_read_file' tcp -U root -d postgres -c "select pg_read_file('/etc/passwd')"
refused 'COPY to a file' tcp -U root -d postgres -c "copy (select 1) to '$RUNNER_TEMP/pwn'"
refused 'COPY from a file' tcp -U root -d postgres -c "create temp table t (l text); copy t from '/etc/passwd'"
refused 'permission denied for function lo_import' tcp -U root -d postgres -c "select lo_import('/etc/passwd')"
refused 'permission denied for function lo_export' tcp -U root -d postgres -c "select lo_export(1, '$RUNNER_TEMP/pwn')"
[ ! -e "$RUNNER_TEMP/pwn" ] || fail "root wrote a file"
tcp -U root -d postgres -c 'create database app'
for extension in dblink postgres_fdw plpython3u; do
  refused 'permission denied to create extension' tcp -U root -d app -c "create extension $extension"
done
refused 'permission denied for language c' tcp -U root -d app -c "create function sh(text) returns int as 'libc.so.6', 'system' language c"

step "a Laravel-like app database: tables, Arabic full-text search, pg_trgm"
tcp -U root -d app -c 'create table posts (id bigserial primary key, body text not null)'
tcp -U root -d app -c "insert into posts (body) values ('مرحبا بالعالم'), ('hello world')"
[ "$(tcp -U root -d app -c "select count(*) from posts where to_tsvector('simple', body) @@ plainto_tsquery('simple', 'بالعالم')")" = 1 ] ||
  fail "Arabic full-text search"
# pg_trgm is a trusted extension: the owner of the database may create it.
tcp -U root -d app -c 'create extension pg_trgm'
# PostgreSQL 16 and later have no `lc_ctype` setting to show; the database
# keeps it.
[ "$(tcp -U root -d app -c "select datctype from pg_database where datname = current_database()")" = C.UTF-8 ] ||
  fail "LC_CTYPE is not C.UTF-8"
# Under LC_CTYPE=C, pg_trgm finds no letter outside ASCII: the similarity
# of two Arabic words was 0, and 0.57 with C.UTF-8.
[ "$(tcp -U root -d app -c "select similarity('مرحبا', 'مرحب') > 0.3")" = t ] || fail "pg_trgm on Arabic"
[ "$(tcp -U root -d app -c "select lower('ÄÖ ABC')")" = 'äö abc' ] || fail "lower() outside ASCII"
# The text search parser classifies letters by the C library's LC_CTYPE:
# Arabic with diacritics and the Arabic comma, the same tokens on every OS.
tokens=$(tcp -U root -d app -c "select string_agg(token || ':' || alias, ' ') from ts_debug('simple', 'مَرْحَبًا، بالعالم')")
echo "tokens: $tokens"
[ "$tokens" = 'مَرْحَبًا:word ، :blank بالعالم:word' ] || fail "Arabic words with diacritics"

step "PHP's pdo_pgsql as root with no password, as Laravel's default .env"
run marsa install php 8.4
PHP=$(ls -d "$DATA"/runtimes/php/8.4.*/bin/php | tail -n 1)
"$PHP" -r '
$db = new PDO("pgsql:host=127.0.0.1;port='"$PORT"';dbname=app", "root", "");
$db->exec("create table migrations (id serial primary key, migration varchar(255) not null)");
$db->exec("insert into migrations (migration) values (\x27create_users_table\x27)");
echo $db->query("select count(*) from migrations")->fetchColumn(), "\n";
' | grep -qx 1 || fail "pdo_pgsql"

step "the database browser: app and its tables, none of the server's own, and a close that closes"
# shellcheck source=api.sh
source "$(dirname "$0")/api.sh"
sources=$(api_ok GET /v1/db/sources)
[ "$(jq -r '.servers[] | select(.server == "postgres") | .state' <<<"$sources")" = running ] || fail "sources: $sources"
names=$(jq -c '[.servers[] | select(.server == "postgres") | .databases[].name]' <<<"$sources")
echo "databases: $names"
jq -e 'index("app") != null and all(.[]; IN("postgres", "template0", "template1") | not)' <<<"$names" >/dev/null ||
  fail "databases: $names"
tables=$(api_ok POST /v1/db/tables '{"source": {"kind": "server", "server": "postgres", "database": "app"}}')
echo "tables: $(jq -c '[.tables[] | [.table.schema, .table.name, .kind]]' <<<"$tables")"
jq -e '[.tables[] | select(.table.schema == "public" and .kind == "table") | .table.name] | index("posts") != null and index("migrations") != null' \
  <<<"$tables" >/dev/null || fail "tables: $tables"
jq -e 'all(.tables[]; .table.schema | startswith("pg_") | not)' <<<"$tables" >/dev/null || fail "a system schema is listed"
[ "$(api_status POST /v1/db/tables '{"source": {"kind": "server", "server": "postgres", "database": "postgres"}}')" = "404 db_not_found" ] ||
  fail "a system database was opened"
# Columns, a page, and one whole value, read as text through the API.
source='{"kind": "server", "server": "postgres", "database": "app"}'
posts='{"schema": "public", "name": "posts"}'
table=$(api_ok POST /v1/db/table "{\"source\": $source, \"table\": $posts}")
[ "$(jq -c '[.columns[] | [.name, .key]]' <<<"$table")" = '[["id","primary"],["body",null]]' ] || fail "columns: $table"
page=$(api_ok POST /v1/db/rows "{\"source\": $source, \"table\": $posts, \"offset\": 0, \"count\": true}")
echo "first row: $(jq -c '.rows[0]' <<<"$page")"
[ "$(jq -r '.rows[0][0]' <<<"$page")" = 1 ] || fail "rows: $page"
[ "$(jq -r '.total.exact' <<<"$page")" = true ] || fail "total: $page"
body=$(tcp -U root -d app -c 'select body from posts where id = 1')
value=$(api_ok POST /v1/db/cell "{\"source\": $source, \"table\": $posts, \"column\": \"body\", \"key\": [\"1\"]}")
[ "$(jq -r '.cell' <<<"$value")" = "$body" ] || fail "cell: $value"
missing=$(api_status POST /v1/db/cell "{\"source\": $source, \"table\": $posts, \"column\": \"body\", \"key\": [\"9\"]}")
[ "$missing" = "404 db_row_not_found" ] || fail "a missing row: $missing"
# One connection for each source (the server's catalog, and app), no pool.
# Each is read-only: the daemon reads the setting back before it uses one.
others="select count(*) from pg_stat_activity where application_name = 'marsa'"
[ "$(tcp -U root -d postgres -c "$others")" = 2 ] || fail "$(tcp -U root -d postgres -c "$others") browser connections, not 2"
api_ok POST /v1/db/close >/dev/null
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ "$(tcp -U root -d postgres -c "$others")" = 0 ] && break
  sleep 0.5
done
[ "$(tcp -U root -d postgres -c "$others")" = 0 ] || fail "a browser connection stayed open after close"

step "the running version cannot be uninstalled; its databases stay"
refused 'in use' marsa uninstall postgres "$VERSION"
[ -d "$PG" ] || fail "the running version was removed"

step "a stop with an idle client: SIGINT, done in under 5 s"
mkfifo "$RUNNER_TEMP/idle"
"$PSQL" -X -h 127.0.0.1 -p "$PORT" -U root -d app <"$RUNNER_TEMP/idle" >/dev/null 2>&1 &
IDLE=$!
exec 3>"$RUNNER_TEMP/idle"
sleep 1
[ "$(tcp -U root -d app -c "select count(*) from pg_stat_activity where usename = 'root' and state = 'idle'")" -ge 1 ] ||
  fail "no idle client"
begun=$(date +%s)
run marsa services stop postgres
took=$(($(date +%s) - begun))
echo "the stop took $took s"
[ "$took" -lt 5 ] || fail "the stop took $took s"
exec 3>&-
wait "$IDLE" || true
grep -q 'received fast shutdown request' "$DATA/logs/postgres.log" || fail "no fast shutdown (SIGINT)"
if grep -q 'received smart shutdown request' "$DATA/logs/postgres.log"; then
  fail "a smart shutdown (SIGTERM)"
fi
grep -q 'enabled = true' "$DATA/config/global.toml" && fail "the stop did not turn it off"

step "a stale socket lock whose pid is another program now: refused, nothing signaled"
# It writes down every signal it gets: `sleep` in a script ignores SIGINT and
# SIGQUIT, PostgreSQL's stop signals, so a signal could go unseen.
perl -e '$SIG{$_} = sub { open my $f, ">>", $ARGV[0]; print $f "@_\n" } for qw(INT QUIT TERM HUP); sleep 300' "$RUNNER_TEMP/signals" &
OTHER=$!
printf '%s\n%s\n' "$OTHER" "$DATA/databases/postgres/18" >"$DATA/run/.s.PGSQL.$PORT.lock"
refused 'service_port_busy' marsa services start postgres
kill -0 "$OTHER" || fail "the other program was signaled"
[ ! -e "$RUNNER_TEMP/signals" ] || fail "the other program got $(cat "$RUNNER_TEMP/signals")"
kill "$OTHER"
rm -f "$DATA/run/.s.PGSQL.$PORT.lock"

step "a second start makes no new cluster, and it starts with marsad when on"
run marsa services start postgres
[ "$(grep -c 'making the data folder.*server="postgres"' "$DATA/logs/marsad.log")" = 1 ] || fail "initdb ran twice"
run marsa daemon stop
run marsa daemon start
started=false
for _ in $(seq 1 30); do
  if marsa services | grep -qE '^postgres +running'; then
    started=true
    break
  fi
  sleep 1
done
[ "$started" = true ] || fail "postgres did not start with marsad"
[ "$(tcp -U root -d app -c 'select count(*) from posts')" = 2 ] || fail "the data did not survive"
run marsa services stop postgres

step "PASS"
