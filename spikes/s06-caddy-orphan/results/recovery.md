# Recovery (macOS arm64, Caddy v2.11.7)

## What each detection method says, and what the pure `decide` function returns

| Case | Setup | pid file (name + start time) | admin API (`/id/marsa-srv`) | port bind + `lsof` (site / admin) | Decision |
| --- | --- | --- | --- | --- | --- |
| R1 | orphan of ours, pid file intact | ours(pid=N) | ours | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |
| R2 | orphan of ours, pid file missing | missing | ours | busy(caddy(N)) / busy(caddy(N)) | reap(pid=Some(N)) |
| R3 | pid file stale (Caddy dead) | stale(pid=N) | no-reply | free / free | start-fresh(remove_pidfile=true) |
| R4 | pid file names a reused pid (a `sleep`) | mismatch(pid=N,found=/bin/sleep) | no-reply | free / free | start-fresh(remove_pidfile=true) |
| R5 | foreign Caddy on both ports | missing | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5b | pid file names a foreign Caddy (same binary, wrong start time) | mismatch(pid=N,found=<repo>/spikes/s06-caddy-orphan/vendor/caddy (start time Wed Nov  4 00:40:54 2026 differs from Mon Jan  1 00:00:00 2024)) | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5c | same as R5b, but the pid file check compares only the path (no start time) | ours(pid=N) | foreign | busy(caddy(N)) / busy(caddy(N)) | conflict(a Caddy that is not ours answers on our admin port) |
| R5d | pid file names a foreign Caddy of our binary on other ports, and a program holds our site port | mismatch(pid=N,found=<repo>/spikes/s06-caddy-orphan/vendor/caddy (start time Wed Nov  4 00:40:55 2026 differs from Mon Jan  1 00:00:00 2024)) | no-reply | busy(s06(N)) / free | conflict(the site port is held by s06(N), which is not our Caddy) |
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
| R1 | reap | decided[]@91ms > stop_api[http=200]@93ms > ports_free[after=stop_api]@93ms > new_caddy_ready[pid=N]@150ms > serving[body="marsa-spike v2"]@150ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 1181 ms |
| R2 | reap | decided[]@102ms > stop_api[http=200]@102ms > ports_free[after=stop_api]@102ms > new_caddy_ready[pid=N]@160ms > serving[body="marsa-spike v2"]@160ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 1185 ms |
| R3 | reap | decided[]@3ms > removed_stale_pidfile[]@4ms > new_caddy_ready[pid=N]@63ms > serving[body="marsa-spike v2"]@64ms | exit 0, new Caddy ready, served "marsa-spike v2" | -; bystander: n/a | 1092 ms |
| R4 | reap | decided[]@3ms > removed_stale_pidfile[]@3ms > new_caddy_ready[pid=N]@60ms > serving[body="marsa-spike v2"]@61ms | exit 0, new Caddy ready, served "marsa-spike v2" | -; bystander: untouched, alive | 1086 ms |
| R5 | reap | decided[]@55ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 65 ms |
| R5b | reap | decided[]@60ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 70 ms |
| R5c | reap | decided[]@57ms | exit 3, CONFLICT a Caddy that is not ours answers on our admin port | -; bystander: untouched, alive | 67 ms |
| R5d | reap | decided[]@30ms | exit 3, CONFLICT the site port is held by s06(N), which is not our Caddy | -; bystander: untouched, alive | 39 ms |
| R5e | reap | decided[]@31ms > stop_api[error=error sending request for url (http://127.0.0.1:2106/stop)]@31ms > sigterm[pid=N sent=true]@2054ms > sigkill[pid=N sent=false]@7073ms | exit 1, 1 signal(s) refused | -; bystander: KILLED | 9108 ms |
| R6 | reap | decided[]@25ms | exit 3, CONFLICT the site port is held by s06(N), which is not our Caddy | -; bystander: untouched, alive | 34 ms |
| R7 | reap | decided[]@1124ms > stop_api[timeout]@3126ms > sigterm[pid=N sent=true]@3141ms > sigkill[pid=N sent=true]@8178ms > ports_free[after=sigkill]@8205ms > new_caddy_ready[pid=N]@8273ms > serving[body="marsa-spike v2"]@8274ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9302 ms |
| R7i | reap | decided[]@1098ms > stop_api[timeout]@3100ms > sigterm[pid=N sent=true]@3112ms > sigkill[pid=N sent=true]@8130ms > ports_free[after=sigkill]@8157ms > new_caddy_ready[pid=N]@8227ms > serving[body="marsa-spike v2"]@8227ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9262 ms |
| R1 | adopt | decided[]@102ms > load[http=200]@105ms > serving[body="marsa-spike v2"]@105ms | exit 0, orphan adopted, served "marsa-spike v2" | orphan kept: alive in 1 of 1 checks; bystander: n/a | 1140 ms |
| R2 | adopt | decided[]@94ms > load[http=200]@95ms > serving[body="marsa-spike v2"]@96ms | exit 0, orphan adopted, served "marsa-spike v2" | orphan kept: alive in 1 of 1 checks; bystander: n/a | 1118 ms |
| R7 | adopt | decided[]@1130ms > stop_api[timeout]@3132ms > sigterm[pid=N sent=true]@3145ms > sigkill[pid=N sent=true]@8166ms > ports_free[after=sigkill]@8192ms > new_caddy_ready[pid=N]@8260ms > serving[body="marsa-spike v2"]@8261ms | exit 0, new Caddy ready, served "marsa-spike v2" | orphan gone; bystander: n/a | 9304 ms |
