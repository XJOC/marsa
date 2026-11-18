#!/usr/bin/env bash
# S-12a: what does building PostgreSQL ourselves take? Runs inside an
# AlmaLinux 8 container (glibc 2.28, the same floor as MySQL's generic build).
#
#   build-pg.sh <version> <out-dir>
#
# Prints and writes build.tsv: time per step, archive and install sizes, the
# glibc symbol floor, and the shared libraries the result needs.
set -eu
VER="${1:-17.12}"
OUT="${2:-/out}"
mkdir -p "$OUT"
rec() { printf '%s\t%s\n' "$1" "$2" | tee -a "$OUT/build.tsv"; }
: >"$OUT/build.tsv"
t() { date +%s; }

t0=$(t)
dnf -y -q install gcc make bison flex perl openssl-devel zlib-devel lz4-devel libicu-devel curl bzip2 tar gzip binutils >/dev/null
rec step_install_build_tools_s $(($(t) - t0))
rec container "$(. /etc/os-release && echo "$PRETTY_NAME"), glibc $(ldd --version | head -1 | awk '{print $NF}'), gcc $(gcc -dumpversion), $(nproc) CPUs"

cd /tmp
t0=$(t)
curl -fsSL -O "https://ftp.postgresql.org/pub/source/v$VER/postgresql-$VER.tar.bz2"
curl -fsSL -O "https://ftp.postgresql.org/pub/source/v$VER/postgresql-$VER.tar.bz2.sha256"
sha256sum -c "postgresql-$VER.tar.bz2.sha256" && rec source_sha256_published_and_ok yes
rec source_tarball_bytes "$(wc -c <"postgresql-$VER.tar.bz2")"
tar xjf "postgresql-$VER.tar.bz2"
rec step_download_source_s $(($(t) - t0))

cd "postgresql-$VER"
t0=$(t)
./configure --prefix=/opt/pg --with-openssl --with-lz4 --with-icu --without-readline >/tmp/configure.log 2>&1
rec step_configure_s $(($(t) - t0))
t0=$(t)
make -j"$(nproc)" >/tmp/make.log 2>&1
rec step_make_s $(($(t) - t0))
t0=$(t)
make install >/tmp/install.log 2>&1
rec step_install_s $(($(t) - t0))

cd /opt
rec install_bytes "$(du -sk /opt/pg | cut -f1)000"
tar czf "$OUT/pg-$VER-local.tar.gz" pg
rec archive_tar_gz_bytes "$(wc -c <"$OUT/pg-$VER-local.tar.gz")"
rec glibc_needed_max "$(objdump -T /opt/pg/bin/postgres | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
rec shared_libs_needed "$(ldd /opt/pg/bin/postgres | awk '{print $1}' | grep -v '^linux-vdso' | tr '\n' ' ')"

# A start and a query, as a normal user (postgres refuses to run as root).
useradd -m pgtest
chown -R pgtest /opt/pg
su pgtest -c '/opt/pg/bin/initdb -D /tmp/pgdata -A trust -E UTF8 --locale=C >/tmp/initdb.log 2>&1 && /opt/pg/bin/pg_ctl -D /tmp/pgdata -o "-p 54329 -c listen_addresses=127.0.0.1" -l /tmp/pg.log start >/dev/null && sleep 2 && /opt/pg/bin/psql -h 127.0.0.1 -p 54329 -U postgres -Atc "select version()"; /opt/pg/bin/pg_ctl -D /tmp/pgdata stop -m fast >/dev/null' | tee /tmp/smoke.txt || true
rec smoke "$(head -1 /tmp/smoke.txt)"
