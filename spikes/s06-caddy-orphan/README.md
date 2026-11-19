# Spike S-06: Caddy orphan recovery (macOS)

Throwaway code. It answers one question: when the daemon dies, does Caddy
survive, and can the next daemon take it back? It is a separate workspace and
never joins the product workspace. Results and the recommendation are in
`docs/decisions/0009-caddy-orphan-recovery.md` (local) and in `results/`.

## Run it

```sh
cd spikes/s06-caddy-orphan
scripts/fetch-caddy.sh                 # downloads Caddy into vendor/ (not committed), checks sha512
cargo build && cargo build --profile abort

scripts/run-matrix.sh matrix           # part 1: D1..D8 x 8 option combinations -> results/death.md
scripts/run-matrix.sh idle             # 30 s idle check on a dead stdout/stderr pipe
scripts/run-matrix.sh stdin            # P4: stdin pipe as a guard
scripts/run-recovery.sh recovery       # part 2: R1..R7 -> results/recovery.md
scripts/run-recovery.sh gap            # request gap, 5 runs each -> results/gap.md
scripts/run-recovery.sh pidfile        # P5: Caddy's own --pidfile
```

Ports: the site is on `127.0.0.1:18106` (`S06_APP_PORT`) and Caddy's admin API
on `127.0.0.1:2106` (`S06_ADMIN_PORT`). Everything binds `127.0.0.1`. A script
stops at once, and kills nobody, if one of the ports is busy.

## What the program does

| Command | Role |
| --- | --- |
| `s06 parent` | plays `marsad`: starts Caddy from a tokio task that owns the `Child`, prints `READY`, dies as asked (`--die`) |
| `s06 recover` | plays the next start: inspects (pid file, admin API, port), decides with a pure function, then `reap` or `adopt` |
| `s06 probe` | one request every 25 ms, reports the longest stretch of failures |
| `s06 squat` | holds a port without serving anything (case R6) |

## Safety

- Caddy's state lives in `work/` (`XDG_CONFIG_HOME`, `XDG_DATA_HOME`), never in
  `~/Library/Application Support/Caddy`.
- Scripts signal only pids they recorded, and only if the process path is one of
  the three binaries of this folder. No `pkill`, no `sudo`.
- `s06 recover` signals a pid only after checking that `ps -o comm=` equals the
  path of the Caddy in `vendor/`, and checks again right before each signal.
