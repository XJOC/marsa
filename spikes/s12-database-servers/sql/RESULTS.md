# S-12a results: MySQL 8.4 and PostgreSQL 17/18 as ready-made downloads

Facts from GitHub runners, 2026-11-14 to 2026-11-18. Nothing was run on the owner's machine. No ADR here: it is written later.

Runs (all in `XJOC/marsa`, workflow `spike-s12-sql`):

- **Last green run, all 14 jobs plus the 2 source builds: [44298396306](https://github.com/XJOC/marsa/actions/runs/44298396306)** (commit "fix Windows log reads, run the full matrix again"). One cell inside it has no final status: `zonky-pg18` on Windows (see "What failed").
- [44291080425](https://github.com/XJOC/marsa/actions/runs/44291080425): PostgreSQL source build numbers, and the Linux/macOS numbers before the last Windows fixes.
- [43628032335](https://github.com/XJOC/marsa/actions/runs/43628032335): first complete Linux/macOS run (the port-conflict test was invalid there, see below).

Artifacts of each run hold one `<candidate>.tsv`, `.log`, and the server logs per candidate. Scripts: `run.sh`, `run.ps1`, `tool.py`, `candidates.txt`.

Runners: `macos-26` and `macos-15` (arm64), `macos-15-intel`, `ubuntu-22.04` (glibc 2.35), `ubuntu-24.04` (2.39), `ubuntu-24.04-arm`, `windows-2022`. `macos-14` is gone: GitHub refuses the job ("The configuration 'macos-14-arm64-us-default' is not supported", run 43627445849). So nothing was tested on macOS 14 or older; `minos` values below are the only evidence.

## 1. Sources, checksums, licenses

| Server | Platform | Source URL (pattern) | Version tested | Checksum / signature the source publishes |
| --- | --- | --- | --- | --- |
| MySQL 8.4 | linux-x64 | `https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-8.4.13-linux-glibc2.28-x86_64.tar.xz` (and `-minimal.tar.xz`) | 8.4.13 | `<file>.md5` and `<file>.asc` (GPG) next to it. **No SHA-256.** |
| MySQL 8.4 | linux-arm64 | `.../mysql-8.4.13-linux-glibc2.28-aarch64.tar.xz`. **No minimal build for arm64** (404 for 8.4.10 to 8.4.13). | 8.4.13 | same |
| MySQL 8.4 | darwin-arm64 | `.../mysql-8.4.13-macos15-arm64.tar.gz` | 8.4.13 | same |
| MySQL 8.4 | darwin-x64 | `https://cdn.mysql.com/archives/mysql-8.4/mysql-8.4.11-macos15-x86_64.tar.gz`. **8.4.13 has no Intel build** (404; the download page lists only arm64 for macOS). 8.4.12 does not exist on the CDN at all. | 8.4.11 | same |
| MySQL 8.4 | windows-x64 | `.../mysql-8.4.13-winx64.zip` | 8.4.13 | same |
| PostgreSQL 17, 18 | all five | theseus-rs: `https://github.com/theseus-rs/postgresql-binaries/releases/download/17.12.0/postgresql-17.12.0-<target>.tar.gz` (Windows: `.zip`). Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`. | 17.12.0, 18.7.0 | `<file>.sha256` per asset; per-asset manifest and SBOM; GitHub artifact attestations (`gh attestation verify`, not run here). No GPG. |
| PostgreSQL 17, 18 | all five | zonky (Maven Central): `https://repo1.maven.org/maven2/io/zonky/test/postgres/embedded-postgres-binaries-<linux-amd64\|linux-arm64v8\|darwin-amd64\|darwin-arm64v8\|windows-amd64>/17.11.0/embedded-postgres-binaries-<p>-17.11.0.jar` (a zip holding `postgres-*.txz`) | 17.11.0, 18.6.0 (one release behind theseus) | `.sha256`, `.sha1`, `.md5`, and `.asc` (GPG, key id 6B02ED0ED53F9FD3, not fetched from a key server in the run) |
| PostgreSQL 17 | windows-x64, darwin | EDB: `https://get.enterprisedb.com/postgresql/postgresql-17.12-1-windows-x64-binaries.zip`, `...-osx-binaries.zip` | 17.12-1 | Windows: `.sha256` exists. macOS: `.sha256` and `.md5` answer AccessDenied, **nothing published**. **No Linux build:** every `linux-x64-binaries.tar.gz` from 17.6 to 18.7 answers 403. |
| PostgreSQL 17 | darwin | Postgres.app: `https://github.com/PostgresApp/PostgresApp/releases/download/v2.9.7/Postgres-2.9.7-17.dmg` | 2.9.7 (PG 17.12) | GitHub release asset `digest` (sha256) only |

All URLs above returned HTTP 200 in the runs, and every downloaded file matched its published checksum (`checksum_ok = yes`), except the two cases with nothing published (EDB macOS: none).

**GPG for MySQL:** the signature is valid on Linux and macOS ("good signature, key BCA43417C3B485DD128EC6D4B7B3B788A8D3785C"). The key file `https://repo.mysql.com/RPM-GPG-KEY-mysql-2023` has the same fingerprint but is **expired** (2025-10-22); `.../RPM-GPG-KEY-mysql-2025` has the same fingerprint, expires 2027-10-23, and verifies the 8.4.13 files. The Windows script did not manage to verify the signature (the key file there was still the expired one; fixed in the script after that run, not re-run on Windows).

**Where old versions live (MySQL):** `cdn.mysql.com/Downloads/MySQL-8.4/` holds only the current release. Older ones are at `cdn.mysql.com/archives/mysql-8.4/<file>` (also `https://downloads.mysql.com/archives/get/p/23/file/<file>`, a 302 to the same place). The `.md5` and `.asc` files exist in both places.

**Licenses (sources, not legal advice):**

- MySQL Server: GPL-2.0. The old "FOSS License Exception" page says it is deprecated (replaced by the Universal FOSS Exception for 8.0.4 and later), covers only the client libraries and connectors, and "does not apply to the MySQL database server" ([page](https://www.mysql.com/about/legal/licensing/foss-exception/)). Marsa would run `mysqld` as a separate process, not link it. Oracle's trademark policy is linked from that page and was not read. Mirroring the binaries needs the GPL-2.0 source offer.
- PostgreSQL: PostgreSQL License, permissive; copyright notice and the two disclaimer paragraphs must stay in all copies ([page](https://www.postgresql.org/about/licence/)). theseus archives ship `COPYRIGHT`, `LICENSE`, and `dependency-notices/`. The theseus repository itself is under the same style of license (its `LICENSE`).
- Not checked: zonky's and EDB's own terms for the repackaging (zonky's repository README states Apache-2.0 for the project, not read in full); Postgres.app bundles PostGIS (GPL-2.0 according to the project; not verified).

## 2. Size and memory

"RSS" is the sum over the server's process tree right after start, empty, 3 s after ready, default config. "Small" is the same with `performance_schema=OFF`, a 64 MB buffer pool (MySQL) or `shared_buffers=32MB`, 20 connections (PostgreSQL). Linux PSS (shared pages counted once) is in the `.tsv` files; for PostgreSQL it is far below RSS (about 22 to 31 MB on Linux).

### MySQL 8.4

| Platform (runner) | Archive | Unpacked | RSS idle | RSS small config | Ready after start |
| --- | --- | --- | --- | --- | --- |
| linux-x64 full (24.04 / 22.04) | 834 MB | 1,574 MB | 449 / 448 MB | 148 MB | 0.7 to 1.3 s |
| linux-x64 **minimal** | **80 MB** | 424 MB | 451 MB | 150 MB | 0.85 s |
| linux-arm64 full | 824 MB | 1,586 MB | 449 MB | 146 MB | 0.88 s |
| darwin-arm64 (macos-15 / macos-26) | 165 MB | 668 MB | 526 / 499 MB | 222 / 190 MB | 0.8 to 1.1 s |
| darwin-x64 (8.4.11) | 172 MB | 678 MB | 461 MB | 160 MB | 1.4 s |
| windows-x64 | 281 MB | 1,263 MB | 446 MB (working set; private 664 MB) | 146 MB | 1.3 s |

The minimal archive has `mysqld`, `mysql`, and `mysqladmin` (the same client tools as the full one). Download time on the runner was 2 to 5 s for the 834 MB file.

### PostgreSQL (MB)

| Source | Platform | Archive | Unpacked | RSS idle (17 / 18) | Small config (17 / 18) |
| --- | --- | --- | --- | --- | --- |
| theseus | linux-x64 | 40 / 41 | 121 / 122 | 70 / 98 | 63 / 89 |
| theseus | linux-arm64 | 40 / 40 | 134 / 136 | 68 / 96 | 61 / 86 |
| theseus | darwin-arm64 | 90 / 89 | 273 / 273 | 36 / 49 | 29 / 41 |
| theseus | darwin-x64 | 93 / 92 | 275 / 274 | 23 / 31 | 16 / 22 |
| theseus | windows-x64 | 49 / 50 | 127 / 129 | 70 / 103 | 62 / 94 |
| zonky | linux-x64 | 15 / 15 | 62 / 63 | 53 / 75 | 46 / 66 |
| zonky | linux-arm64 | 14 / 14 | 62 / 63 | 52 / 74 | 45 / 64 |
| zonky | darwin-arm64 | 62 / 71 | 311 / 341 | 38 / 52 | 31 / 44 |
| zonky | darwin-x64 | 62 / 71 | 313 / 341 | 25 / 33 | 18 / 24 |
| zonky | windows-x64 | 24 / 25 | 103 / 109 | 74 / 108 (18: no final status) | 66 / 100 |
| EDB (17 only) | windows-x64 | 386 | 1,008 | 73 | 66 |
| EDB (17 only) | darwin (universal) | 449 | 1,264 to 1,274 | 25 to 42 | 18 to 34 |
| Postgres.app (17 only) | darwin (universal) | 120 | 399 | 24 to 37 | 16 to 29 |

PostgreSQL ready time: 50 to 150 ms on Linux and macOS (up to 690 ms on some macOS runs), about 200 to 630 ms on Windows. Process count 6 (9 on 18).

## 3. Does it run (one table of verdicts)

| Server / source | linux-x64 | linux-arm64 | darwin-arm64 | darwin-x64 | windows-x64 |
| --- | --- | --- | --- | --- | --- |
| MySQL 8.4 (Oracle) | works **if `libaio.so.1` exists** (missing on ubuntu-24.04; present on ubuntu-22.04 runner) | same | works | works (8.4.11 only) | works |
| PostgreSQL theseus | works on glibc 2.36 or newer. **Fails on Ubuntu 22.04**: `GLIBC_2.36 not found` | same | works (needs macOS 15, see below) | works (macOS 15) | works (via `pg_ctl start`) |
| PostgreSQL zonky | works (glibc 2.25 for 17, 2.27 for 18) | works | works | works | works (via `pg_ctl start`) |
| PostgreSQL EDB | no Linux download | no Linux download | works | works | works (via `pg_ctl start`) |
| PostgreSQL Postgres.app | n/a | n/a | works | works | n/a |

Smoke query on every working cell: create table, insert an Arabic string, select it back over TCP; the server reported `127.0.0.1` only. Windows smoke used an ASCII string (the script passes arguments through the Windows command line).

## 4. Running without admin rights

All of this ran as a normal user (Linux, macOS), or as the runner's administrator account on Windows (see the PostgreSQL note). No `sudo` except the runner setup that is not part of the product path.

**Loopback only.** Every server was started with an explicit bind (`--bind-address=127.0.0.1 --mysqlx=OFF` for MySQL; `listen_addresses=127.0.0.1` for PostgreSQL). Listening sockets (lsof, ss, Get-NetTCPConnection) were `127.0.0.1:<port>` only in every cell. MySQL's X protocol port (33060) was not tested with the default on; it is switched off by `--mysqlx=OFF` (not verified what it would bind by default). Unix sockets: MySQL `mysql.sock` and `mysql.sock.lock`; PostgreSQL `.s.PGSQL.<port>` and `.lock` (dot files; Windows has none).

**Non-default and default ports.** A free high port worked everywhere. Starting on 3306 / 5432 also worked on every runner (the ports were free there; both are above 1024). When another program held the port (a Python listener on 127.0.0.1:port), every server exited with code 1 within 0.2 to 3 s and printed the reason: MySQL `Can't start server: Bind on TCP/IP port: Address already in use` (Windows: `Only one usage of each socket address ... is normally permitted`), PostgreSQL `could not bind IPv4 address "127.0.0.1": Address already in use` (FATAL, exit 1). The first complete run (43628032335) showed "kept running" because the test's listener could not bind (TIME_WAIT); that was a bug in the test, fixed.

**Memory/idle client facts.** MySQL idles at about 450 to 520 MB with the default config (performance schema and a 128 MB buffer pool), 146 to 222 MB with the small config. PostgreSQL idles at 23 to 108 MB.

### Stopping (with one idle client connected)

| Server | Method | Result |
| --- | --- | --- |
| MySQL | `mysqladmin shutdown` | exits 0 in 2.6 s (macOS), 3.2 s (Linux arm64), 0.7 to 2.6 s (Windows) |
| MySQL | `SIGTERM` | exits 0 in 1.0 s (macOS), 3.4 s (Linux arm64); log says `Shutdown complete`; next start has no recovery |
| MySQL | `SIGKILL` / `TerminateProcess` | exit 137; pid file and socket files stay; the next start runs `XA crash recovery` and is ready in 1.2 to 1.7 s |
| PostgreSQL | `pg_ctl stop -m fast` | exits 0 in 0.13 s |
| PostgreSQL | `SIGINT` (fast) | exits 0 in 0.25 s |
| PostgreSQL | **`SIGTERM` (smart)** | **still running after 20 s** while the idle client stays connected, on every platform (Windows: `pg_ctl stop -m smart`). It needed `SIGINT` to end. |
| PostgreSQL | `SIGQUIT` / `pg_ctl -m immediate` | exits at once; next start logs `database system was not properly shut down; automatic recovery in progress`; ready in about 0.36 s |
| PostgreSQL | `SIGKILL` / `TerminateProcess` | exit 137; `postmaster.pid` stays; next start recovers the same way |

Windows has no signals: the tested methods were `mysqladmin`, `pg_ctl -m fast|smart|immediate`, and `TerminateProcess`.

### Orphan (parent killed with SIGKILL / TerminateProcess)

The server was started as a child of a wrapper; the wrapper was killed; the server was not told.

- **The server keeps running** (Linux, macOS: parent pid becomes 1; Windows: still alive). It keeps the port and keeps answering queries. Both MySQL and PostgreSQL left the **pid file in place** (`mysqld.pid`, `postmaster.pid`, first line = server pid, matched the real pid on Linux and macOS) and the **unix socket files** (`mysql.sock`, `mysql.sock.lock`, `.s.PGSQL.<port>`, `.lock`).
- **A second start on the same data dir:** PostgreSQL refuses at once (`FATAL: lock file "postmaster.pid" already exists`, 30 to 400 ms, exit 1). **MySQL does not fail fast:** InnoDB retries the lock for about 100 s (100.4 to 105.5 s measured on Linux and macOS) and then aborts with `Data Dictionary initialization failed`, exit 1. On Windows MySQL refused too (`Failed to initialize DD Storage Engine`, exit 1); the log timestamps put it near 14 s (not measured by the script).
- **Stopping the orphan through its pid file:** read the pid, send `SIGTERM` (MySQL) or `SIGINT` (PostgreSQL); both exited cleanly, and the pid file and sockets were removed. On Windows: `mysqladmin shutdown` / `pg_ctl stop -m fast` worked the same way.
- **Everything killed at once** (parent and server `SIGKILL`): pid file and socket files stay, the port is free, the next start works. MySQL ready in 1.2 to 1.7 s with `XA crash recovery`; PostgreSQL in about 0.37 s with automatic recovery. A stale pid file did not block either server.
- **Windows PostgreSQL note:** the server was started with `pg_ctl start` because the runner is an administrator and `postgres.exe` refuses to run in an administrator account (message below). `pg_ctl` returns after the start, so the "parent" in the Windows PostgreSQL orphan test is `pg_ctl`'s launcher, not a long-lived supervisor; that cell does not test what happens when a supervisor that holds the `postgres.exe` process dies. A real user is not an administrator when Marsa runs.

## 5. macOS

| Check | MySQL 8.4 (Oracle) | theseus | zonky | EDB | Postgres.app |
| --- | --- | --- | --- | --- | --- |
| Runs after a plain `curl` download (no quarantine attribute; `com.apple.quarantine` count = 0) | yes | yes | yes | yes | yes |
| Architectures | arm64 only (arm64 build) / x86_64 only (Intel build) | arm64 or x86_64 (separate archives) | universal x86_64+arm64 | universal | universal |
| Minimum macOS (`LC_BUILD_VERSION minos`) | 14.0 | **15.0** | 12.0 (17), 13.0 (18) | 12.0 | 10.15 |
| Code signature | Developer ID Application: Oracle America, Inc. (VB5E2TV963), hardened runtime | **ad hoc** (no Team ID) | Developer ID Application: EnterpriseDB Corporation (26QKX55P9K) | same as zonky | Developer ID Application: Jakob Egger (ZF84SJ5A3G) |
| Libraries outside the system | none missing; bundled `libssl.3`, `libcrypto.3`, `libprotobuf` | none (`dylib_other_absolute` empty) | bundled | bundled | some libraries (boost, PostGIS side) reference `/Applications/Postgres.app/Contents/Versions/17/lib/...` by absolute path; the server itself ran from another folder |

`otool -L` on `mysqld`'s own list is in the `.tsv` (`main_binary_deps`, added after the first run, so only in the later runs). The otool check lists absolute paths outside `/usr/lib` and `/System`; Homebrew is installed on the runners, so a missing library could be hidden. The bundled-library lists show no `/opt/homebrew` or `/usr/local` reference except MySQL's `/usr/local/mysql/lib/libfido2.1.dylib` (a client-auth plugin library, not needed by `mysqld` to start).

macOS 14 and older were **not tested** (runner retired). By `minos`, theseus' macOS builds need macOS 15 or newer. Gatekeeper behaviour on a quarantined download was not tested (we download without the attribute).

## 6. Linux

- **MySQL generic builds** need glibc 2.28 (highest symbol `GLIBC_2.28`, `GLIBCXX_3.4.22`). Their own libraries are inside the archive; `ldd` lists `libpthread`, `librt`, `libssl.so.3` and others as resolved from the archive's `lib/private`.
- **`libaio.so.1` is a system dependency.** `ldd` says `mysqld: libaio.so.1 => not found` on `ubuntu-24.04` and `ubuntu-24.04-arm` (the package there is `libaio1t64`), and `mysqld --version` fails with `error while loading shared libraries: libaio.so.1` (run 43627445849). The `ubuntu-22.04` runner already had `libaio1`. Workaround used in the runs, no root: `apt-get download libaio1t64` (14 KB to 67 KB `.deb`, unpacked with `dpkg-deb -x`), a `libaio.so.1` symlink, and `LD_LIBRARY_PATH`. With it everything above passed. In Docker images (`ubuntu:22.04`, `debian:12`, `almalinux:9`) a bare `mysqld --version` also failed on `libaio.so.1` (those checks ran without the workaround in the first runs and with the library directory mounted in later ones; see the `compat_*` rows in the `.tsv`). Alpine: `no such file` (glibc binary).
- **theseus glibc builds need glibc 2.36** (built on Debian 12; the bundled `libstdc++` asks for it). Verified: `postgres --version` fails on `ubuntu:22.04` (glibc 2.35) and `almalinux:9` (2.34) with `version GLIBC_2.36 not found`, works on `debian:12`; and `init failed` on the `ubuntu-22.04` runner (runs 43627445849, 44298396306). theseus also publishes musl builds (dynamically linked, need musl); they were not run here.
- **zonky glibc builds** need glibc 2.25 (PG 17) / 2.27 (PG 18); they bundle `libssl.so.1.1`, `libcrypto.so.1.1` (OpenSSL 1.1.1, end of life), ICU 60, `libxml2`, `libxslt`. They contain `postgres`, `initdb`, `pg_ctl` only: **no `psql`**, no `pg_dump`.
- theseus and EDB/Postgres.app archives contain the full client tools (`psql`, `pg_dump`, ...).

## 7. Windows

- Downloads were fetched with `curl.exe`: no Mark of the Web (`Zone.Identifier` absent).
- MySQL zip: executables are Authenticode-signed by Oracle America, Inc. (Valid). The zip is 281 MB, 1,263 MB unpacked, 296 files, and includes `.pdb` files. No `vcruntime*.dll` in `bin` (the runner has the Visual C++ runtime installed, so whether a clean Windows would run it was **not tested**).
- theseus and zonky Windows `postgres.exe`: **NotSigned**. EDB: signed (EnterpriseDB Corporation, Valid). None bundles the VC runtime either.
- **`postgres.exe` refuses to run in an administrator account:** `Execution of PostgreSQL by a user with administrative permissions is not permitted. The server must be started under an unprivileged user ID` (theseus 17 and 18, run 44292771781). `initdb.exe` ran; `pg_ctl start` also works as administrator because it drops the rights itself. The runner is an administrator, so the Windows PostgreSQL cells use `pg_ctl start`.
- `Start-Process` on `mysqld.exe` returned a process that was not the server in one run; the pid file holds the real pid (the script was changed to trust it).
- `mysqld.exe` listens on `127.0.0.1:<port>` only with the options above. Named pipe and shared memory are off by default.

## 8. What building ourselves takes (measured: PostgreSQL only)

PostgreSQL 17.12 from source in an `almalinux:8` container (glibc 2.28, gcc 8, 4 CPUs), run 44291080425:

| | x64 | arm64 |
| --- | --- | --- |
| Install build tools (dnf) | 28 s | 35 s |
| Download source (22.3 MB `.tar.bz2`) | 4 s | 5 s |
| `configure` | 8 s | 13 s |
| `make -j4` | 63 s | 73 s |
| `make install` | 1 s | 2 s |
| Install size / `tar.gz` | 34 MB / 10.7 MB | 34 MB / 10.7 MB |
| Highest glibc symbol | 2.25 | 2.25 |

`./configure --with-openssl --with-lz4 --with-icu --without-readline`. The result links the system `libssl.so.1.1`, `libcrypto.so.1.1`, `libicu*.so.60`, `liblz4`, `libz` dynamically, so a portable archive would need those libraries bundled (as zonky does) or static. The PostgreSQL project publishes `postgresql-17.12.tar.bz2.sha256` next to the source tarball (checked in the run: ok). The in-container start-and-query step printed nothing (the `su` call failed; not diagnosed), so that build was **not smoke-tested**.

MySQL from source: **not measured.** The gaps that would need it are linux-arm64 minimal (no 80 MB archive; the only arm64 download is 824 MB) and darwin-x64 after 8.4.11. No build was tried.

## 9. What failed and why

| What | Why / evidence |
| --- | --- |
| `macos-14` runner | GitHub refuses it (`macos-14-arm64-us-default is not supported`). Replaced by `macos-26`. No macOS 14 result exists. |
| MySQL on Ubuntu 24.04 | `libaio.so.1` missing; `mysqld` cannot start (run 43627445849). Worked after a private copy of libaio. |
| theseus PostgreSQL on Ubuntu 22.04 | needs glibc 2.36 (runs 43628032335, 44291080425, 44298396306). Containers `ubuntu:22.04` and `almalinux:9` fail the same way. |
| MySQL 8.4.13 on Intel macOS | no such download. 8.4.11 is the last Intel build found (8.4.12 is absent). |
| MySQL linux-arm64 minimal | no such download for 8.4.10 to 8.4.13. |
| EDB on Linux | every `linux-x64-binaries.tar.gz` URL answers 403. |
| `postgres.exe` as administrator | refuses to start; worked through `pg_ctl start`. |
| Windows script, first runs | `-h127.0.0.1` was split by PowerShell (fixed with `--host=`); `-Filter` given an array; GNU `tar` from Git read `D:` as a host and hung the job on the zonky jar (fixed with 7-Zip); `Get-Content` got a `$null` path. Runs 43628032335 (job cancelled at 30 min) and 44292771781. |
| Port conflict test, first complete run | the test listener could not bind (TIME_WAIT); fixed with `SO_REUSEADDR` off Windows; valid from run 44291080425 on. |
| Windows PostgreSQL orphan cells, last run | In `zonky-pg18` the log ends at the "everything killed at once" step: `orphan_hardkill_port_held = yes` and the pid file was still alive, so the kill hit the `pg_ctl` launcher, not the postmaster, and the script then stopped without a final `status` (no `TIMEOUT` line was written; not diagnosed). The other Windows PostgreSQL cells show `orphan_hardkill_restart = ok`. The Windows PostgreSQL orphan results are therefore weak (see section 4). |
| In-container PostgreSQL build smoke test | printed nothing; not diagnosed. |
| GPG on Windows | MySQL key import used the expired key file at that time; zonky key not on the runner. Verified on Linux/macOS only. |

## 10. Open questions

- macOS 14 and older: untestable on GitHub runners now. Does theseus' `minos` 15.0 (ad hoc signed) and MySQL's 14.0 hold on real Macs?
- Windows without the Visual C++ runtime: not tested (the runner has it).
- Does `gh attestation verify` pass for theseus archives? Not run. zonky's GPG key fetch not done in the runs.
- MySQL 8.4 `mysql_native_password` is off by default in 8.4; a Laravel/PDO connection was not tested (the smoke client was the bundled `mysql` binary).
- A second `mysqld` on the same data dir takes about 100 s to fail: Marsa needs its own check (pid file or port) before starting.
- Idle MySQL memory (450 to 520 MB default) versus PostgreSQL (23 to 108 MB): the small config numbers are one configuration, not a tuned one.
- MySQL source build time (arm64, Intel macOS): not measured.
- Redistribution/mirroring terms for zonky, EDB, Postgres.app, and Oracle's trademark policy: not read.
- Windows PostgreSQL orphan behaviour with a real supervisor as parent (not `pg_ctl`): not tested; run on a non-admin account.
