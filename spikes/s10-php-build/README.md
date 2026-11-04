# S-10: build our own static PHP

Question: can we build and host a static PHP with the extension set Laravel needs?
The prebuilt `static-php.dev` sets do not fit: `bulk` has no `pdo_sqlite` or
`pdo_pgsql`; `common` has no `opcache` or `intl`.

Do not run this on a laptop with a full disk. It builds on GitHub Actions:
`.github/workflows/spike-s10-php-build.yml` (push to `task/S-10-*`, or run by hand).

| File | What it does |
| --- | --- |
| `extensions.txt` | the static extensions, one spc name per line |
| `shared-extensions.txt` | extensions built as a shared `.so` (`xdebug`) |
| `expected-modules.txt` | what `php -m` must list |
| `spc.sha256` | sha256 of the pinned spc 2.8.5 release assets |
| `build.sh` | `spc doctor`, `spc download`, `spc build`; writes the timings |
| `smoke.sh` | `php -v`, `php -m` vs the list, zend symbol count, Xdebug `.so`, runtime checks, a FastCGI request through php-fpm |
| `runtime-checks.php` | SQLite, intl, gd, zip, sodium, redis class, ... (PASS, FAIL, INFO lines) |
| `fcgi-get.php` | a 100-line FastCGI client used by `smoke.sh` |
| `laravel-check.sh` | `composer create-project laravel/laravel`, migrate, `artisan test` |
| `report.sh` | job summary and one table row per platform |

Results are in the job summaries and in ADR 0012 (`docs/decisions/`).
The binaries are workflow artifacts (`php-<platform>`, 14 days). No release is created.
