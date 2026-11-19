# Prevention (Linux)

- idle check, stdio pipe, parent SIGKILLed, no requests:
  - +1 s: alive(ppid=1)
  - +10 s: alive(ppid=1)
  - +20 s: alive(ppid=1)
  - +30 s: alive(ppid=1)
  - after the first request: alive(ppid=1)
- stdin pipe held by the parent, parent SIGKILLed: Caddy alive(ppid=1) after 3 s

## P7: PR_SET_PDEATHSIG (Linux x86_64, Ubuntu 22.04.5 LTS)

Caddy gets SIGTERM when the thread that spawned it ends. The parent keeps its tokio runtime.

| Case | spawned from | parent | Caddy +1 s | Caddy +5 s | ppid of Caddy |
| --- | --- | --- | --- | --- | --- |
| P7a | task | kill9 | dead | dead | - |
| P7b | task | term | dead | dead | - |
| P7c | task | none | alive(ppid=11213) | alive(ppid=11213) | 11213:s06 |
| P7d | short-thread | none | START FAILED: Error: Caddy did not answer on the admin API within 10s | | |

## systemd --user (Linux x86_64, Ubuntu 22.04.5 LTS, systemd 249 (249.11-0ubuntu3.23))

The parent runs as a transient user unit and is SIGKILLed. `kill_on_drop` is on, so only systemd can kill Caddy here.

| Case | unit | KillMode | Caddy +1 s | Caddy +5 s | ppid of Caddy | unit after |
| --- | --- | --- | --- | --- | --- | --- |
| U1 | service | control-group | dead | dead | - | failed |
| U2 | service | mixed | dead | dead | - | failed |
| U3 | service | process | alive(ppid=939) | alive(ppid=939) | 939:systemd | failed |
| U4 | scope | - | alive(ppid=1) | alive(ppid=1) | 1:systemd | inactive |
# P5: Caddy's `--pidfile` against ours
- Caddy's file exists at READY: yes; content: `15796` (just the pid, no path, no start time)
- after SIGKILL of the parent: file kept; Caddy alive(ppid=1)
- after SIGTERM of Caddy (clean exit): file removed
- after SIGKILL of Caddy: file kept (stale)
- our file is on disk 8422 microseconds after `spawn` returned (includes one `ps` call for the start time)
- Caddy's own file appeared 29 ms after `exec` (run 1)
- Caddy's own file appeared 30 ms after `exec` (run 2)
- Caddy's own file appeared 29 ms after `exec` (run 3)
