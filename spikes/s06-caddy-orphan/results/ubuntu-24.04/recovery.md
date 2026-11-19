# Recovery (Linux x86_64, Ubuntu 24.04.5 LTS, Caddy v2.11.7)

## What each detection method says, and what the pure `decide` function returns

| Case | Setup | pid file (name + start time) | admin API (`/id/marsa-srv`) | port bind + `lsof` (site / admin) | Decision |
| --- | --- | --- | --- | --- | --- |
| R1 | orphan of ours, pid file intact | ours(pid=N) | ours | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |
| R2 | orphan of ours, pid file missing | missing | ours | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |
| R3 | pid file stale (Caddy dead) | stale(pid=N) | no-reply | free / free | start-fresh(remove_pidfile=true) |
| R4 | pid file names a reused pid (a `sleep`) | mismatch(pid=N,found=/usr/bin/sleep) | no-reply | free / free | start-fresh(remove_pidfile=true) |
| R5 | foreign Caddy on both ports | missing | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5b | pid file names a foreign Caddy (same binary, wrong start time) | mismatch(pid=N,found=/home/runner/work/marsa/marsa/spikes/s06-caddy-orphan/vendor/caddy (start time Thu Nov 19 09:52:05 2026 differs from Mon Jan  1 00:00:00 2024)) | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5c | same as R5b, but the pid file check compares only the path (no start time) | ours(pid=N) | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5d | pid file names a foreign Caddy of our binary on other ports, and a program holds our site port | mismatch(pid=N,found=/home/runner/work/marsa/marsa/spikes/s06-caddy-orphan/vendor/caddy (start time Thu Nov 19 09:52:06 2026 differs from Mon Jan  1 00:00:00 2024)) | no-reply | busy(s06(N)) / free | conflict(the site port is held by s06(N), which is not our Caddy) |
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
| R1 | reap | decided[]@33ms > stop_api[http=200]@34ms > ports_free[after=stop_api]@34ms > new_caddy_ready[pid=N]@67ms > serving[body="marsa-spike v2"]@68ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 1080 ms |
| R2 | reap | decided[]@26ms > stop_api[http=200]@28ms > ports_free[after=stop_api]@29ms > new_caddy_ready[pid=N]@62ms > serving[body="marsa-spike v2"]@63ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 1075 ms |
| R3 | reap | decided[]@5ms > removed_stale_pidfile[]@5ms > new_caddy_ready[pid=N]@39ms > serving[body="marsa-spike v2"]@39ms | exit 0, new Caddy ready, served "marsa-spike v2" | -; bystander: n/a | 1052 ms |
| R4 | reap | decided[]@5ms > removed_stale_pidfile[]@6ms > new_caddy_ready[pid=N]@39ms > serving[body="marsa-spike v2"]@39ms | exit 0, new Caddy ready, served "marsa-spike v2" | -; bystander: untouched, alive | 1052 ms |
| R5 | reap | decided[]@18ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 26 ms |
| R5b | reap | decided[]@30ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 38 ms |
| R5c | reap | decided[]@23ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 29 ms |
| R5d | reap | decided[]@21ms | exit 3, CONFLICT the site port is held by s06(N), which is not our Caddy | -; bystander: untouched, alive | 28 ms |
| R5e | reap | decided[]@18ms > stop_api[error=error sending request for url (http://127.0.0.1:2106/stop)]@18ms > sigterm[pid=N sent=true]@2031ms > sigkill[pid=N sent=false]@7048ms | exit 1, 1 signal(s) refused | -; bystander: KILLED | 9069 ms |
| R6 | reap | decided[]@16ms | exit 3, CONFLICT the site port is held by s06(N), which is not our Caddy | -; bystander: untouched, alive | 23 ms |
| R7 | reap | decided[]@1040ms > stop_api[timeout]@3041ms > sigterm[pid=N sent=true]@3043ms > sigkill[pid=N sent=true]@8062ms > ports_free[after=sigkill]@8089ms > new_caddy_ready[pid=N]@8123ms > serving[body="marsa-spike v2"]@8124ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9138 ms |
| R7i | reap | decided[]@1029ms > stop_api[timeout]@3031ms > sigterm[pid=N sent=true]@3032ms > sigkill[pid=N sent=true]@8050ms > ports_free[after=sigkill]@8050ms > new_caddy_ready[pid=N]@8084ms > serving[body="marsa-spike v2"]@8085ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9097 ms |
| R1 | adopt | decided[]@37ms > load[http=200]@39ms > serving[body="marsa-spike v2"]@39ms | exit 0, orphan adopted, served "marsa-spike v2" | orphan kept: alive in 1 of 1 checks; bystander: n/a | 1055 ms |
| R2 | adopt | decided[]@27ms > load[http=200]@29ms > serving[body="marsa-spike v2"]@29ms | exit 0, orphan adopted, served "marsa-spike v2" | orphan kept: alive in 1 of 1 checks; bystander: n/a | 1045 ms |
| R7 | adopt | decided[]@1041ms > stop_api[timeout]@3041ms > sigterm[pid=N sent=true]@3043ms > sigkill[pid=N sent=true]@8067ms > ports_free[after=sigkill]@8067ms > new_caddy_ready[pid=N]@8101ms > serving[body="marsa-spike v2"]@8102ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9112 ms |
