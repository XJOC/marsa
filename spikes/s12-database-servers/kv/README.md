# S-12b: a key-value server (Valkey) for Marsa, and Redis on Windows

Spike code, not product code. Questions and answers are in `RESULTS.md`. The
task is S-12 (`docs/tasks/S-12-database-servers.md`, kept outside the repo).

Everything runs on GitHub runners through `.github/workflows/spike-s12-kv.yml`
(push to a `task/S-12b-*` branch, or run it by hand). **Do not run these
scripts on your own machine**: they start servers, and the Garnet job renames
the .NET folder and the Memurai job installs an MSI.

## What runs

| Part | Where | What |
| --- | --- | --- |
| `unix/kv.sh` | macOS and Linux runners | `obtain` (official tarball, or own build from source, or a static musl build in Alpine), `probe` (start on 6379 and on 6390, memory, listening address, stop by `valkey-cli shutdown` and by SIGTERM, data dir, orphan after SIGKILL of the parent), `laravel`, `summary` |
| `windows/kv.ps1` | `windows-2022` | the same four steps for Garnet, Memurai, tporadowski/redis, redis-windows builds, and our own Valkey builds |
| `windows/build-valkey.sh` | MSYS2 and Cygwin shells | builds Valkey 8.1.11 from the source tarball and records the errors if it does not build |
| `laravel/run.sh`, `laravel/run.ps1` | all | `composer create-project laravel/laravel`, `predis/predis`, then the probes below |
| `laravel/files/app/...` | copied into the app | `kv:cache`, `kv:dispatch`, `kv:verify` commands and four job classes |
| `tools/resp.php` | all | a RESP client with no dependencies (`PING`, `INFO`, `SHUTDOWN`) so every server is probed the same way |
| `tools/aggregate.py` | the `report` job | one markdown report from all artifacts |

The Laravel app uses `REDIS_CLIENT=predis`, `CACHE_STORE=redis`,
`QUEUE_CONNECTION=redis`, so no PHP extension is needed. The probes:

- cache: put/get (strings, arrays), has/forget, increment/decrement, add (`SET NX`), remember, forever,
  many/putMany, tags, locks (basic, closure, owner restore, block with a wait), rate limiter, the default
  cache connection on DB 1 (`SELECT`), raw hash/list/set/zset, pipeline, `MULTI/EXEC`, Lua `EVAL` and
  `EVALSHA`, `SCAN`, TTL expiry;
- queue: 5 immediate jobs, a named queue, a delayed job (3 s), a job that fails once and succeeds on retry
  (backoff 2 s), a job that always fails (2 tries, lands in `failed_jobs`, `failed()` called), a
  `ShouldBeUnique` job dispatched twice, `Queue::size`, and a second run with `block_for=1` (`BLPOP`).
  The worker is `php artisan queue:work redis --queue=high,default --stop-when-empty` run twice, 5 s apart.

## Notes on the matrix

- `macos-14` was the label in the brief, but GitHub marks the macOS 14 image deprecated (retired
  2026-11-02, see `actions/runner-images` README). `macos-15` is arm64 and `macos-15-intel` is the Intel
  label that exists now.
- Valkey 8 means 8.1.11 here (latest 8.x; 8.0.12 also exists). Valkey 9.0.7 and 9.1.3 are current.
