# Request gap during recovery (Linux x86_64, Ubuntu 24.04.5 LTS, Caddy v2.11.7)

A probe sends one HTTP request every 25 ms to the site (300 ms timeout) while `s06 recover`
handles an orphan (case R1). The gap is the time from the last good answer before the failures
to the first good answer after them, so it can be up to one probe interval longer than the real outage.

## reap, 5 runs

| Run | gap (ms) | failed requests | reap done at (ms) | decided at (ms) |
| --- | --- | --- | --- | --- |
| 1 | 52 | 1 | 76 | 41 |
| 2 | 0 | 0 | 76 | 41 |
| 3 | 53 | 1 | 75 | 40 |
| 4 | 53 | 1 | 75 | 41 |
| 5 | 0 | 0 | 74 | 39 |

- median gap: 52 ms, max gap: 53 ms
- median failed requests: 1, max: 1
- median time to finish: 75 ms; median time to decide: 41 ms

## adopt, 5 runs

| Run | gap (ms) | failed requests | adopt done at (ms) | decided at (ms) |
| --- | --- | --- | --- | --- |
| 1 | 0 | 0 | 42 | 41 |
| 2 | 0 | 0 | 45 | 43 |
| 3 | 0 | 0 | 40 | 39 |
| 4 | 0 | 0 | 49 | 47 |
| 5 | 0 | 0 | 43 | 41 |

- median gap: 0 ms, max gap: 0 ms
- median failed requests: 0, max: 0
- median time to finish: 43 ms; median time to decide: 41 ms

## Detection time per method (all 10 runs above)

| Method | median (ms) | max (ms) |
| --- | --- | --- |
| pidfile | 10.55 | 11.0 |
| admin | 0.75 | 1.2 |
| port | 29.95 | 36.6 |
