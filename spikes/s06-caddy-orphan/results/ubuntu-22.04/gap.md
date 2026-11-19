# Request gap during recovery (Linux x86_64, Ubuntu 22.04.5 LTS, Caddy v2.11.7)

A probe sends one HTTP request every 25 ms to the site (300 ms timeout) while `s06 recover`
handles an orphan (case R1). The gap is the time from the last good answer before the failures
to the first good answer after them, so it can be up to one probe interval longer than the real outage.

## reap, 5 runs

| Run | gap (ms) | failed requests | reap done at (ms) | decided at (ms) |
| --- | --- | --- | --- | --- |
| 1 | 53 | 1 | 215 | 179 |
| 2 | 80 | 2 | 217 | 178 |
| 3 | 53 | 1 | 217 | 181 |
| 4 | 52 | 1 | 219 | 182 |
| 5 | 79 | 2 | 220 | 183 |

- median gap: 53 ms, max gap: 80 ms
- median failed requests: 1, max: 2
- median time to finish: 217 ms; median time to decide: 181 ms

## adopt, 5 runs

| Run | gap (ms) | failed requests | adopt done at (ms) | decided at (ms) |
| --- | --- | --- | --- | --- |
| 1 | 0 | 0 | 187 | 186 |
| 2 | 0 | 0 | 185 | 184 |
| 3 | 0 | 0 | 197 | 195 |
| 4 | 0 | 0 | 195 | 193 |
| 5 | 0 | 0 | 192 | 191 |

- median gap: 0 ms, max gap: 0 ms
- median failed requests: 0, max: 0
- median time to finish: 192 ms; median time to decide: 191 ms

## Detection time per method (all 10 runs above)

| Method | median (ms) | max (ms) |
| --- | --- | --- |
| pidfile | 13.5 | 13.8 |
| admin | 1 | 1.0 |
| port | 169.65 | 181.2 |
