# Prevention checks (macOS arm64)

## P4: stdin pipe as a guard

- stdin pipe held by the parent, parent SIGKILLed: Caddy alive(ppid=1) after 3 s

## P3 follow-up: idle Caddy on a dead stdout/stderr pipe

- idle check, stdio pipe, parent SIGKILLed, no requests:
  - +1 s: alive(ppid=1)
  - +10 s: alive(ppid=1)
  - +20 s: alive(ppid=1)
  - +30 s: alive(ppid=1)
  - after the first request: alive(ppid=1)

## P5: Caddy's `--pidfile` against ours
- Caddy's file exists at READY: yes; content: `32206` (just the pid, no path, no start time)
- after SIGKILL of the parent: file kept; Caddy alive(ppid=1)
- after SIGTERM of Caddy (clean exit): file removed
- after SIGKILL of Caddy: file kept (stale)
- our file is on disk 3813 microseconds after `spawn` returned (includes one `ps` call for the start time)
- Caddy's own file appeared 31 ms after `exec` (run 1)
- Caddy's own file appeared 38 ms after `exec` (run 2)
- Caddy's own file appeared 34 ms after `exec` (run 3)
