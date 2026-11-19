# Request gap during recovery (macOS arm64, Caddy v2.11.7)

A probe sends one HTTP request every 25 ms to the site (300 ms timeout) while `s06 recover`
handles an orphan (case R1). The gap is the time from the last good answer before the failures
to the first good answer after them, so it can be up to one probe interval longer than the real outage.

## reap, 5 runs

| Run | gap (ms) | failed requests | reap done at (ms) | decided at (ms) |
| --- | --- | --- | --- | --- |
| 1 | 54 | 1 | 164 | 107 |
| 2 | 54 | 1 | 167 | 110 |
| 3 | 54 | 1 | 166 | 108 |
| 4 | 55 | 1 | 181 | 123 |
| 5 | 54 | 1 | 171 | 114 |

- median gap: 54 ms, max gap: 55 ms
- median failed requests: 1, max: 1
- median time to finish: 167 ms; median time to decide: 110 ms

## adopt, 5 runs

| Run | gap (ms) | failed requests | adopt done at (ms) | decided at (ms) |
| --- | --- | --- | --- | --- |
| 1 | 0 | 0 | 111 | 110 |
| 2 | 0 | 0 | 93 | 92 |
| 3 | 0 | 0 | 110 | 109 |
| 4 | 0 | 0 | 118 | 116 |
| 5 | 0 | 0 | 114 | 113 |

- median gap: 0 ms, max gap: 0 ms
- median failed requests: 0, max: 0
- median time to finish: 111 ms; median time to decide: 110 ms

## Detection time per method (all 10 runs above)

| Method | median (ms) | max (ms) |
| --- | --- | --- |
| pidfile | 13.8 | 22.7 |
| admin | 1.35 | 1.6 |
| port | 96.85 | 99.5 |
