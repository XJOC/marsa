| State | bench wall ms | vs 0 | bench CPU ms | vs 0 | startup ms | vs 0 (ms) | PHP peak MB | max RSS MB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 no Xdebug (baseline) | 283.3 | +0.0% | 283.0 | +0.0% | 31.9 | +0.0 | 34.0 | 59.3 |
| 1 loaded, mode=off | 285.1 | +0.6% | 284.8 | +0.6% | 32.2 | +0.3 | 34.0 | 59.8 |
| 2 loaded, mode=develop | 888.9 | +213.8% | 888.0 | +213.8% | 32.4 | +0.5 | 34.0 | 60.0 |
| 3 loaded, mode=debug, start_with_request=no | 765.7 | +170.3% | 764.7 | +170.2% | 32.5 | +0.6 | 34.0 | 60.1 |
| 4 loaded, mode=debug, start_with_request=yes, no listener | 767.5 | +170.9% | 766.2 | +170.8% | 32.5 | +0.5 | 34.0 | 60.1 |

Median ms per workload part (bench.php): loop = 12M iterations, no calls; calls = fib(30); arrays; strings.

| State | loop | calls | arrays | strings |
| --- | ---: | ---: | ---: | ---: |
| 0 no Xdebug (baseline) | 128 (1.0x) | 85 (1.0x) | 44 (1.0x) | 26 (1.0x) |
| 1 loaded, mode=off | 128 (1.0x) | 85 (1.0x) | 45 (1.0x) | 27 (1.0x) |
| 2 loaded, mode=develop | 297 (2.3x) | 301 (3.5x) | 189 (4.3x) | 100 (3.8x) |
| 3 loaded, mode=debug, start_with_request=no | 323 (2.5x) | 218 (2.6x) | 159 (3.6x) | 66 (2.5x) |
| 4 loaded, mode=debug, start_with_request=yes, no listener | 322 (2.5x) | 218 (2.6x) | 160 (3.7x) | 66 (2.5x) |

Each cell is a median: bench = 5 runs, startup = 100 runs.
bench wall ms min..max per state: 0: 281..284; 1: 284..286; 2: 881..892; 3: 764..771; 4: 764..773
