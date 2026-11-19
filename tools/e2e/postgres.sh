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
DATA=${RUNNER_TEMP:-/tmp}/pg-e2e
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
refused 'pg_hba.conf' tcp -U postgres -d postgres -c 'select 1'
[ "$(sock -U postgres -d postgres -c 'select rolsuper from pg_roles where rolname = current_user')" = t ] ||
  fail "postgres over the socket"
refused 'pg_hba.conf' sock -U root -d postgres -c 'select 1'
# Mode 0700: only the owner may use the socket.
ls -la "$DATA/run" | grep -E "^srwx------ .* \.s\.PGSQL\.$PORT\$" || fail "the socket is not 0700"

step "root cannot run programs or read files as the owner"
refused 'permission denied' tcp -U root -d postgres -c "copy (select 1) to program 'id'"
refused 'permission denied' tcp -U root -d postgres -c "select pg_read_file('/etc/passwd')"
tcp -U root -d postgres -c 'create database app'
for extension in dblink postgres_fdw plpython3u; do
  # Not trusted, or not in this build: refused either way.
  refused 'ERROR' tcp -U root -d app -c "create extension $extension"
done

step "a Laravel-like app database: tables, Arabic full-text search, pg_trgm"
tcp -U root -d app -c 'create table posts (id bigserial primary key, body text not null)'
tcp -U root -d app -c "insert into posts (body) values ('مرحبا بالعالم'), ('hello world')"
[ "$(tcp -U root -d app -c "select count(*) from posts where to_tsvector('simple', body) @@ plainto_tsquery('simple', 'بالعالم')")" = 1 ] ||
  fail "Arabic full-text search"
# pg_trgm is a trusted extension: the owner of the database may create it.
tcp -U root -d app -c 'create extension pg_trgm'
[ "$(tcp -U root -d app -c "select similarity('hello', 'hell') > 0.3")" = t ] || fail "pg_trgm"
# Measured, not required: initdb runs with LC_CTYPE=C (task M6-03, part B),
# under which pg_trgm finds no Arabic letters. A database of its own with
# LC_CTYPE=C.UTF-8 shows whether that fixes it on this system.
echo "pg_trgm on Arabic, LC_CTYPE=C: $(tcp -U root -d app -c "select similarity('مرحبا', 'مرحب')")"
echo "lower() under the cluster's locale: $(tcp -U root -d app -c "select lower('ÄÖ ABC')")"
if tcp -U root -d postgres -c "create database utf8 template template0 locale_provider builtin builtin_locale 'C.UTF-8' lc_collate 'C' lc_ctype 'C.UTF-8'"; then
  tcp -U root -d utf8 -c 'create extension pg_trgm'
  echo "pg_trgm on Arabic, LC_CTYPE=C.UTF-8: $(tcp -U root -d utf8 -c "select similarity('مرحبا', 'مرحب')")"
else
  echo "no database with LC_CTYPE=C.UTF-8 on this system"
fi

step "PHP's pdo_pgsql as root with no password, as Laravel's default .env"
run marsa install php 8.4
PHP=$(ls -d "$DATA"/runtimes/php/8.4.*/bin/php | tail -n 1)
"$PHP" -r '
$db = new PDO("pgsql:host=127.0.0.1;port='"$PORT"';dbname=app", "root", "");
$db->exec("create table migrations (id serial primary key, migration varchar(255) not null)");
$db->exec("insert into migrations (migration) values (\x27create_users_table\x27)");
echo $db->query("select count(*) from migrations")->fetchColumn(), "\n";
' | grep -qx 1 || fail "pdo_pgsql"

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
sleep 300 &
OTHER=$!
printf '%s\n%s\n' "$OTHER" "$DATA/databases/postgres/18" >"$DATA/run/.s.PGSQL.$PORT.lock"
refused 'service_port_busy' marsa services start postgres
kill -0 "$OTHER" || fail "the other program was signaled"
kill "$OTHER"
rm -f "$DATA/run/.s.PGSQL.$PORT.lock"

step "a second start makes no new cluster, and it starts with marsad when on"
run marsa services start postgres
[ "$(grep -c 'making the PostgreSQL data folder' "$DATA/logs/marsad.log")" = 1 ] || fail "initdb ran twice"
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
