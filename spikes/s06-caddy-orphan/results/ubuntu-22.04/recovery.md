# Recovery (Linux x86_64, Ubuntu 22.04.5 LTS, Caddy v2.11.7)

## What each detection method says, and what the pure `decide` function returns

| Case | Setup | pid file (name + start time) | admin API (`/id/marsa-srv`) | port bind + `lsof` (site / admin) | Decision |
| --- | --- | --- | --- | --- | --- |
| R1 | orphan of ours, pid file intact | ours(pid=N) | ours | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |
| R2 | orphan of ours, pid file missing | missing | ours | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |
| R3 | pid file stale (Caddy dead) | stale(pid=N) | no-reply | free / free | start-fresh(remove_pidfile=true) |
| R4 | pid file names a reused pid (a `sleep`) | mismatch(pid=N,found=/usr/bin/sleep) | no-reply | free / free | start-fresh(remove_pidfile=true) |
| R5 | foreign Caddy on both ports | missing | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5b | pid file names a foreign Caddy (same binary, wrong start time) | mismatch(pid=N,found=/home/runner/work/marsa/marsa/spikes/s06-caddy-orphan/vendor/caddy (start time Thu Nov 19 09:52:30 2026 differs from Mon Jan  1 00:00:00 2024)) | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5c | same as R5b, but the pid file check compares only the path (no start time) | ours(pid=N) | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5d | pid file names a foreign Caddy of our binary on other ports, and a program holds our site port | mismatch(pid=N,found=/home/runner/work/marsa/marsa/spikes/s06-caddy-orphan/vendor/caddy (start time Thu Nov 19 09:52:32 2026 differs from Mon Jan  1 00:00:00 2024)) | no-reply | busy(s06(N)) / free | conflict(the site port is held by s06(N), which is not our Caddy) |
| R5e | same as R5d, but the pid file check compares only the path (no start time) | ours(pid=N) | no-reply | busy(s06(N)) / free | reap(pid=Some(N)) |
| R6 | another program holds the site port | missing | no-reply | busy(s06(N)) / free | conflict(the site port is held by s06(N), which is not our Caddy) |
| R7 | orphan of ours, frozen (SIGSTOP) | ours(pid=N) | timeout | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |
| R7i | same as R7, but the pid file is ignored (pid comes from `lsof` on the admin port) | missing | timeout | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |
| R1 | orphan of ours, pid file intact | ours(pid=N) | ours | busy(caddy(N)) / busy(caddy(N)) | adopt(pid=Some(N)) |
| R2 | orphan of ours, pid file missing | missing | ours | busy(caddy(N)) / busy(caddy(N)) | adopt(pid=Some(N)) |
| R7 | orphan of ours, frozen (SIGSTOP) | ours(pid=N) | timeout | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |

## What each action did

Times are milliseconds since the start of `s06 recover`. `hold` is 1 s, so the total includes it.

| Case | Action | Steps | Result | State afterwards | Wall time |
| --- | --- | --- | --- | --- | --- |
| R1 | reap | decided[]@176ms > stop_api[http=200]@177ms > ports_free[after=stop_api]@177ms > new_caddy_ready[pid=N]@215ms > serving[body="marsa-spike v2"]@216ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 1230 ms |
| R2 | reap | decided[]@164ms > stop_api[http=200]@165ms > ports_free[after=stop_api]@165ms > new_caddy_ready[pid=N]@202ms > serving[body="marsa-spike v2"]@203ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 1219 ms |
| R3 | reap | decided[]@7ms > removed_stale_pidfile[]@7ms > new_caddy_ready[pid=N]@42ms > serving[body="marsa-spike v2"]@43ms | exit 0, new Caddy ready, served "marsa-spike v2" | -; bystander: n/a | 1058 ms |
| R4 | reap | decided[]@7ms > removed_stale_pidfile[]@7ms > new_caddy_ready[pid=N]@42ms > serving[body="marsa-spike v2"]@43ms | exit 0, new Caddy ready, served "marsa-spike v2" | -; bystander: untouched, alive | 1059 ms |
| R5 | reap | decided[]@109ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 119 ms |
| R5b | reap | decided[]@122ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 133 ms |
| R5c | reap | decided[]@115ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 125 ms |
| R5d | reap | decided[]@68ms | exit 3, CONFLICT the site port is held by s06(N), which is not our Caddy | -; bystander: untouched, alive | 78 ms |
| R5e | reap | decided[]@61ms > stop_api[error=error sending request for url (http://127.0.0.1:2106/stop)]@61ms > sigterm[pid=N sent=true]@2081ms > sigkill[pid=N sent=false]@7083ms | exit 1, 1 signal(s) refused | -; bystander: KILLED | 9110 ms |
| R6 | reap | decided[]@55ms | exit 3, CONFLICT the site port is held by s06(N), which is not our Caddy | -; bystander: untouched, alive | 65 ms |
| R7 | reap | decided[]@1177ms > stop_api[timeout]@3178ms > sigterm[pid=N sent=true]@3180ms > sigkill[pid=N sent=true]@8184ms > ports_free[after=sigkill]@8211ms > new_caddy_ready[pid=N]@8248ms > serving[body="marsa-spike v2"]@8249ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9264 ms |
| R7i | reap | decided[]@1164ms > stop_api[timeout]@3165ms > sigterm[pid=N sent=true]@3167ms > sigkill[pid=N sent=true]@8171ms > ports_free[after=sigkill]@8197ms > new_caddy_ready[pid=N]@8235ms > serving[body="marsa-spike v2"]@8236ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9250 ms |
| R1 | adopt | decided[]@176ms > load[http=200]@178ms > serving[body="marsa-spike v2"]@178ms | exit 0, orphan adopted, served "marsa-spike v2" | orphan kept: alive in 1 of 1 checks; bystander: n/a | 1198 ms |
| R2 | adopt | decided[]@170ms > load[http=200]@172ms > serving[body="marsa-spike v2"]@173ms | exit 0, orphan adopted, served "marsa-spike v2" | orphan kept: alive in 1 of 1 checks; bystander: n/a | 1193 ms |
| R7 | adopt | decided[]@1180ms > stop_api[timeout]@3182ms > sigterm[pid=N sent=true]@3184ms > sigkill[pid=N sent=true]@8196ms > ports_free[after=sigkill]@8223ms > new_caddy_ready[pid=N]@8260ms > serving[body="marsa-spike v2"]@8261ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9276 ms |
