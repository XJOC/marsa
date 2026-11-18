# S-12b results: Valkey 8 and Redis-compatible servers on Windows

Facts only. The ADR is written elsewhere. Code and scripts are in this folder; every number comes from a GitHub runner.

- Last green run (all 17 cells + report): https://github.com/XJOC/marsa/actions/runs/43630345963 (Windows cells, `windows-2022`).
- Unix cells, same scripts: run https://github.com/XJOC/marsa/actions/runs/43629269893 (also green; its numbers are used below). The first runs (43627746448, 43628430049) failed on a PowerShell parse error, a Valkey Windows build target, and three harness bugs; fixed in later commits.
- Valkey version tested: 8.1.11 (latest 8.x; 8.0.12 exists; 9.0.7 and 9.1.3 are current).
- Runner labels: `macos-14` is deprecated (images retire 2026-11-02, https://github.com/actions/runner-images), so `macos-15` (arm64) and `macos-15-intel` were used.
- Laravel test: fresh `laravel/laravel` + `predis/predis`, `REDIS_CLIENT=predis`, `CACHE_STORE=redis`, `QUEUE_CONNECTION=redis`. 39 checks (cache, tags, locks, rate limiter, SELECT DB 1, pipeline, MULTI/EXEC, EVAL/EVALSHA, SCAN, TTL, queue: immediate, named queue, delayed, retry with backoff, failing job to `failed_jobs`, unique job, BLPOP with `block_for=1`). Details in `README.md`.

## 1. Valkey on macOS and Linux

| Cell | Source | Checksum | Build time | Archive (server+cli) | Unpacked | RSS empty | Laravel |
| --- | --- | --- | --- | --- | --- | --- | --- |
| darwin-arm64 | own build, `make BUILD_TLS=no`, libc malloc | source tarball sha256 matches https://github.com/valkey-io/valkey-hashes | 40 s | 1.06 MB | 2.64 MB | 4.3 MB | 39/39 |
| darwin-x64 | own build, same | same | 71 s | 1.17 MB | 2.75 MB | 3.1 MB | 39/39 |
| linux-x64 official noble | https://download.valkey.io/releases/valkey-8.1.11-noble-x86_64.tar.gz | `.sha256` file next to it, matches | none | 36.5 MB (full set, 6 binaries) | 90.6 MB | 12.4 MB | 39/39 |
| linux-x64 official jammy | `...-jammy-x86_64.tar.gz` | matches | none | 36.1 MB | 88.4 MB | 11.0 MB | 39/39 |
| linux-arm64 official noble | `...-noble-arm64.tar.gz` | matches | none | 36.2 MB | 89.5 MB | 11.1 MB | 39/39 |
| linux-x64 own glibc build (ubuntu-24.04, jemalloc) | source | matches | 80 s | 2.17 MB | 5.46 MB | 7.5 MB | 39/39 |
| linux-arm64 own glibc build | source | matches | 60 s | 2.14 MB | 5.12 MB | 6.5 MB | 39/39 |
| linux-x64 own static musl (Alpine 3.22, `MALLOC=libc LDFLAGS=-static`) | source | matches | 59 s | 1.46 MB | 3.26 MB | 3.5 MB | 39/39 |
| linux-arm64 own static musl | source | matches | 52 s | 1.46 MB | 3.22 MB | 3.4 MB | 39/39 |

Sources and licenses:
- valkey.io publishes binaries for Linux only (Ubuntu jammy and noble, x86_64 and arm64), each with a `.sha256` file; no signature mentioned. No macOS or Windows binaries (https://valkey.io/download/). 8.0.12 and 8.1.11 tarballs exist at the same URL pattern.
- The official tarballs are dynamically linked with libssl3, libcrypto3 and libsystemd0 (TLS and systemd built in). The noble one needs GLIBC 2.38 (fails on Ubuntu 22.04: `version GLIBC_2.38 not found`); the jammy one needs GLIBC 2.34. A 24.04 own glibc build also needs GLIBC 2.38 (it takes the build host's glibc).
- The static musl build is `statically linked`, runs on the Ubuntu hosts, no shared libraries.
- macOS: Homebrew formula `valkey` 9.1.3 only (no 8.x formula). Bottles exist for arm64 macOS and Linux, none for Intel macOS; it depends on `openssl@4` (https://formulae.brew.sh/api/formula/valkey.json). Formula license field: `BSD-3-Clause AND BSD-2-Clause AND BSL-1.0 AND MIT AND (CC0-1.0 OR BSD-2-Clause)`. Bottles are on ghcr.io. Not tested in CI; our own build needs only `libSystem` (see below), so the bottle route was not pursued.
- License of Valkey itself: BSD-3-Clause (https://raw.githubusercontent.com/valkey-io/valkey/8.1.11/COPYING). The official tarball contains `share/LICENSE`.
- Source verification: https://github.com/valkey-io/valkey-hashes lists the sha256 of each source tarball.

macOS binary checks (own build, `MACOSX_DEPLOYMENT_TARGET=12.0`):
- `otool -L`: only `/usr/lib/libSystem.B.dylib`. `minos 12.0`.
- arm64: `codesign` shows `adhoc,linker-signed`. x64: unsigned (not required on Intel).
- No `com.apple.quarantine` after the curl and tar steps (xattr empty). A copy with the quarantine attribute set by hand (`0083;...;Safari;`) still ran from a shell (`--version` rc 0).
- First start to PING: 376 ms (arm64), 518 ms (x64); Linux 62 to 102 ms.

Start, stop, orphan (identical on all nine Unix cells):
- Start with `--bind 127.0.0.1 --port 6379 --save "" --appendonly no`: `lsof` shows only `127.0.0.1:6379`. Port 6390 with `--dir`: `SHUTDOWN` saves `dump.rdb` (111 bytes), restart recovers the key.
- `valkey-cli shutdown nosave`: 32 to 252 ms, exit 0, port free, pid file removed. SIGTERM: 136 to 358 ms, exit 0, port free, pid file removed.
- `--pidfile` is written even without daemonizing.
- Orphan (parent shell SIGKILLed): server stays alive, parent pid becomes 1, port held, pid file stays. A second start on the same port exits with code 1 and `Could not create server TCP listening socket 127.0.0.1:6391: bind: Address already in use`. `valkey-cli shutdown nosave` on the orphan stops it, the port is free, restart works.
- SIGKILL of the server itself: stale pid file stays (old pid), port free at once, restart works and rewrites the pid file.
- `INFO server` gives `process_id`, `tcp_port`, `executable`, `valkey_version`, which identifies an orphan as ours.
- `redis_version` reports `7.2.4` (compatibility value) next to `valkey_version:8.1.11`.

## 2. Windows candidates (`windows-2022`, Windows Server 2022 10.0.20348)

| | Garnet 2.2.12 | Memurai Developer 4.1.2 | tporadowski/redis 5.0.14.1 | redis-windows 7.2.16 msys2 | redis-windows 7.2.16 cygwin | redis-windows 8.10.3 msys2 | Valkey 8.1.11 own, MSYS2 | Valkey 8.1.11 own, Cygwin |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Source | github.com/microsoft/garnet release `win-x64-based-readytorun.zip` | `dist.memurai.com` MSI | github.com/tporadowski/redis release zip | github.com/redis-windows/redis-windows release zip | same | same | built in CI from source | built in CI from source |
| Checksum | GitHub asset `digest` sha256 (matches); no checksum file | sha256 in the winget manifest (matches) | none published; computed `018ea18a...` | GitHub asset `digest` (matches) | matches | matches | source sha256 from valkey-hashes (matches) | matches |
| License | MIT | proprietary: dev and test only, no production, "share or distribute the software" forbidden | BSD-3-Clause (Redis 5.0) | Redis 7.2.16 (BSD line); wrapper repo Apache-2.0 | same | Redis 8.10.3: RSALv2 or SSPLv1 or AGPLv3 (not BSD) | BSD-3-Clause; MSYS runtime LGPL-3.0 | BSD-3-Clause; Cygwin DLL LGPL-3.0+ with linking exception |
| Archive | 26.4 MB zip | 8.8 MB MSI | 12.6 MB zip | 11.0 MB | 11.7 MB | 14.0 MB | 11.7 MB (exe + dll, zipped) | 14.9 MB (dist folder) |
| Unpacked | 85.2 MB (+ 80.7 MB .NET runtime) | n/a (extracted image) | 48.3 MB (with .pdb files) | 30.4 MB | 31.6 MB | 38.7 MB | n/a | n/a |
| Needs | .NET 10 runtime (see below) | `libssl`, `libcrypto`, `memurai-services.dll` shipped in the MSI | nothing | `msys-2.0.dll` + 4 more DLLs | `cygwin1.dll` + 7 more | msys DLLs | `msys-2.0.dll`, `msys-gcc_s-seh-1.dll` | `cygwin1.dll`, `cyggcc_s-seh-1.dll` |
| Start to PING | 625 ms | 608 ms | 619 ms | 610 ms | 611 ms | 608 ms | 587 ms | 587 ms |
| Working set empty | 52 MB (private 195 MB) | 34 MB | 46 MB | 11.8 MB | 11.8 MB | 12.3 MB | 9.9 MB | 9.9 MB |
| Listens on | 127.0.0.1 | 127.0.0.1 | 127.0.0.1 | 127.0.0.1 | 127.0.0.1 | 127.0.0.1 | 127.0.0.1 | 127.0.0.1 |
| Graceful stop over the protocol | `SHUTDOWN` is unknown (`ERR unknown command`); process stays until killed | `SHUTDOWN NOSAVE` 3 ms | 4 ms | 109 ms | 105 ms | 110 ms | 112 ms | 110 ms |
| Pid file | none | yes, Windows pid | yes, Windows pid | yes, but the Cygwin pid, not the Windows pid | same | same | same (1110 vs 8096) | same |
| Data dir recover (`SAVE`, restart) | checkpoint files (`ht.dat.0` 128 MiB sparse); needs `--checkpointdir` and `--recover`; value recovered | recovered | recovered | recovered | recovered | recovered | recovered | recovered |
| Orphan (parent cmd.exe killed) | server stays, port held; second start prints `AddressAlreadyInUse` and exits (exit code 0) | server stays; second start exits 1 | same | same | same | same | same | same |
| After `TerminateProcess` | restart fine | restart fine, stale pid file | same | same | same | same | same | same |
| Started as a standard user (no admin) | yes (local user in `Users` only, server pid owned by that user) | yes | yes | yes | yes | yes | yes | yes |
| Laravel checks | 39/39 | 39/39 | 39/39 | 39/39 | 39/39 | 39/39 | 39/39 | 39/39 |

Notes per candidate:
- **Garnet.** Release assets for Windows (`win-x64-based-readytorun.zip`, `portable.7z`, the dotnet tool nupkg) are framework-dependent. With every .NET folder renamed: `You must install .NET to run this application` (apphost 10.0.15). A private runtime works: `dotnet-runtime-10.0.15-win-x64.zip` from https://builds.dotnet.microsoft.com/dotnet/Runtime/10.0.15/dotnet-runtime-10.0.15-win-x64.zip, 37.0 MB, 80.7 MB unpacked, sha512 matches https://builds.dotnet.microsoft.com/dotnet/release-metadata/10.0/releases.json; start with `DOTNET_ROOT` pointing at it. Lua is off by default: `--lua` is needed (Laravel queues use Lua; https://github.com/microsoft/garnet/blob/main/website/docs/getting-started/configuration.md). Default bind is any address, so `--bind 127.0.0.1` is required. `redis_version` reports 7.4.3. Releases appear almost daily (2.2.7 to 2.2.12 in 10 days).
- **Memurai.** The MSI unpacks without elevation use or a service via `msiexec /a MSI /qn TARGETDIR=dir` (administrative image); `memurai.exe` from that folder runs (`Memurai server v=4.1.2 API v=7.2.5`). The runner is admin, so the MSI step itself was not tested as a standard user. EULA (https://dist.memurai.com/releases/Memurai-Developer/4.1.2/MemuraiDeveloperFiles/Memurai-Developer-EULA.rtf): use "to design, develop, test and demonstrate your programs. You may not use the software in a production environment"; section 6 forbids to "share or distribute the software"; telemetry clause; automatic updates. https://www.memurai.com/get-memurai says production use is prohibited and the Developer edition shuts down after 10 days (not seen in the short runs). Version 4.2+ needs a license file (docs.memurai.com/en/config-license).
- **tporadowski/redis.** Last release 5.0.14.1 (2022-02-17). Master/develop last commit 2021-10-17. Branches `win-6.0`, `win-6.2`, `win-8.10` exist without releases. It passed all 39 Laravel checks, including tags. One script bug: Redis 5 needs `--save 1 1` as two arguments (fixed).
- **redis-windows (MSYS2 and Cygwin Redis builds).** Built with the recipe in https://github.com/redis-windows/redis-windows/blob/main/.github/workflows/build-redis.yml (MSYS2 or Cygwin, `sed` on `dlfcn.h`, `-O0`). Redis 7.4 and later are RSALv2/SSPLv1 (March 2024); Redis 8 adds AGPLv3 (https://redis.io/blog/agplv3/). Redis 7.2.x and 6.2.x builds are in the same release list.
- **Valkey built in MSYS2 and Cygwin.** Valkey 8.1 has no Cygwin or MSYS code, but it builds with `make BUILD_TLS=no OPTIMIZATION=-O2 valkey-server valkey-cli` after one change: `sed -i 's/__GNU_VISIBLE/1/' /usr/include/dlfcn.h` (needed on Cygwin: `Dl_info` and `dladdr` hidden; applied on both). Build time: MSYS2 56 s, Cygwin 133 s on the runner. `valkey-benchmark` does not compile on MSYS2 (`asprintf` implicit declaration), so it is not built. Result: `valkey_version:8.1.11`, multiplexing `select`. MSYS2/Cygwin paths show in `INFO` and `CONFIG GET dir` as POSIX paths (`/_temp/kv-work/...`). `maxclients` is reduced to 3168 (ulimit 3200). Native MinGW build was not tried (no `fork` or `epoll` port).
- **Pid files on MSYS2/Cygwin builds** hold the Cygwin pid, not the Windows pid (e.g. `1110` vs `8096`); a Windows supervisor must use the pid of the process it started.
- **Standard-user test.** A throwaway local user in `Users` only was created on the runner after granting Users the interactive and batch logon rights with `secedit`; every candidate started as that user and listened on 127.0.0.1:6392. The first attempts failed because Windows Server denies standard users local logon by default and `net user` rejected a password over 14 characters (harness issues, not server issues).
- Ports: no excluded range contained 6379 on these runners (ranges seen: 80, 5985-5986, 47001 and two random 100-port blocks that changed per run). A dev machine with Hyper-V or WSL may differ.

## 3. Laravel failures

- Final runs: none (39 of 39 for every cell, Windows and Unix).
- Harness bugs found on the first runs, all fixed: Predis returns numbers as strings (3 checks), `SCAN MATCH` does not get the Predis key prefix (1 check). These failed identically on Valkey/Linux, so they were not server faults.
- Not tested: Redis extension (`phpredis`), Horizon, pub/sub, streams, cluster, ACL/password, TLS, Windows without `--lua` for Garnet (Lua is required by Laravel queues; the flag was always passed).

## 4. What was not done or is uncertain

- `macos-14` as in the brief was not used (deprecated, see above).
- Windows runner is admin; the MSI of Memurai was only extracted, and a real install (`INSTALL_SERVICE=0`) was not needed because the extracted image ran.
- No performance or load test; only empty-server memory.
- Windows ARM64 not tested (`windows-11-arm`). Garnet and the redis-windows project publish no Valkey build.
- Valkey 8.0/8.1/9.x differences were not compared; only 8.1.11.
- Homebrew bottle was not downloaded or run.
