# S-10: build our own static PHP

Question: can we build and host a static PHP with the extension set Laravel needs?
The prebuilt `static-php.dev` sets do not fit: `bulk` has no `pdo_sqlite` or
`pdo_pgsql`; `common` has no `opcache` or `intl`.

The spike is over (ADR 0012). Its build and smoke scripts moved to
`tools/php-build/`, its extension lists to `recipes/php-extensions.txt`, and its
workflow became `.github/workflows/php-build.yml` (task M2-09). The spike
workflow and the three runs that ADR 0012 cites are in the git history.

The scripts left here were not moved, because the pipeline does not run them:

| File | What it does |
| --- | --- |
| `laravel-check.sh` | `composer create-project laravel/laravel`, migrate, `artisan test` (needs the network) |
| `distro-check.sh` | runs the build on other Linux distributions in Docker |
| `report.sh` | the spike's job summary |
