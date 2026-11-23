# Changelog

All notable changes to Marsa are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [0.3.0] - Unreleased

Marsa now installs and runs your database servers, as it does PHP: no Docker,
no Homebrew. The window has a new design, and the menu-bar icon opens a small
panel. Linux and Windows stay a preview.

### Added

- **Database servers.** `marsa install mysql`, `marsa install postgres`, and
  `marsa install valkey`, on macOS, Linux, and Windows.
  - **MySQL 8.4** (8.4.13; 8.4.11 on an Intel Mac, because Oracle has no newer
    Intel build), **PostgreSQL 17 and 18**, and **Valkey 8.1** (a
    Redis-compatible server).
  - The first server you install starts at once, and starts again each time
    Marsa starts. `marsa services start <id>` and `marsa services stop <id>`
    control each one. Each listens on `127.0.0.1` only (ports 3306, 5432, and
    6379; each is a setting in `global.toml`).
  - MySQL and PostgreSQL have a `root` user with no password, and Valkey has
    no password. Their data is in `databases/` in Marsa's data folder, and
    `marsa uninstall` of a server keeps it.
  - MySQL runs with a small memory setting. If `marsad` crashes, the next start
    stops a server that it left running.
  - `marsa services` prints the lines for a Laravel project's settings file
    while a server runs: `DB_CONNECTION`, `DB_HOST`, `DB_PORT`, `DB_USERNAME`,
    and `DB_PASSWORD`, or `REDIS_HOST` and `REDIS_PORT` for Valkey.
  - Marsa's PHP has `pdo_mysql`, `pdo_pgsql`, and the `redis` extension, so a
    new Laravel project can use MySQL or PostgreSQL, and Valkey for its cache
    and queue.
  - Where each server comes from is checked before Marsa publishes it:
    Oracle's GPG signature and MD5 for MySQL, GitHub's build attestation and
    the `.sha256` for PostgreSQL (from the
    [theseus-rs](https://github.com/theseus-rs/postgresql-binaries) builds),
    and the sums of Marsa's own build for Valkey. The list is signed with
    Marsa's key, as before.
  - After you install a server, its client programs are in your `PATH`:
    `mysql`, `mysqldump`, `mysqladmin`, `psql`, `pg_dump`, `pg_restore`,
    `valkey-cli`, and `redis-cli`. They already point to Marsa's server (and
    never change a variable you set yourself). Marsa makes them only after you
    install a server, so it does not hide a `psql` from Homebrew.
  - `marsa install` picks the newest version that has files for your system,
    and says so when your operating system is too old for a server
    (`install_os_too_old`).
- **A new window.** Home, Sites (and the page of one site), Runtimes (one
  screen for every runtime, databases too), Services (with a log for each),
  Logs, and Settings, in Marsa's new design. A first-run screen walks you
  through the first site. The window follows your system's light or dark
  setting.
- **A menu-bar panel.** Click the icon: a small panel opens with your sites,
  your PHP version, Xdebug, Open, Stop, and Quit. Click again to close it. It
  shows no Dock icon on macOS. On Linux the icon opens a menu with the same
  items.
- **Linux.** `marsa setup` also trusts Marsa's certificate authority in Chrome
  and Firefox (their own certificate stores). `marsa uninstall --all` now runs
  on Linux and removes only the entries of this data folder. If `marsad`
  crashes, the next start finds the Caddy and PHP-FPM it left running.
- **Doctor.** `marsa doctor`, the window's Settings, and the daemon use one
  list of checks. `marsa doctor` also checks that `/usr/bin/xz`, which MySQL
  needs on Linux, is there.
- A DNS port that another program holds shows as a failed `dns` service,
  with the reason.

### Fixed

- The last PHP stays installed while a site runs it, also when no default is
  set.
- The administrator helper on macOS no longer follows a link that stands in
  the place of a file it works on.
- `marsa setup` waits until Caddy's certificate file is complete, and does not
  start Caddy too early.
- No request from the command line or the window waits more than 30 seconds
  for the daemon.

### Known limits

- Linux and Windows are a preview: no person has tried them yet. That holds
  for the database servers too: they are tested end to end on GitHub runners.
- The window does not browse databases yet. Use `mysql`, `psql`, and
  `valkey-cli`, or your own client.
- MySQL has no Intel Mac build after 8.4.11 (Oracle's choice).
- The macOS programs are signed ad hoc, not notarized by Apple. The Windows
  programs are not code-signed yet, so SmartScreen may warn.
- Without `minisign`, the install scripts check only the SHA-256 of the
  download, not its signature.

## [0.2.0] - 2026-11-18

Linux and Windows join macOS, as a preview: they are tested end to end on
GitHub runners, from the built programs to an open Laravel page, but no person
has tried them yet. macOS stays the stable system.

### Added

- **Linux (preview),** x64 and arm64. `install.sh` installs it. `marsa setup`
  runs once through the system's admin prompt (pkexec): it sends `.test`
  names to Marsa (through systemd-resolved, or through `/etc/hosts` where that
  is not safe, and then `marsa park` and `marsa link` add new sites there),
  lets Caddy use ports 80 and 443, and trusts Marsa's certificate authority.
- **Windows (preview),** x64. One PowerShell line installs it for your user,
  with no admin rights, and puts Marsa in your user `PATH`:
  `irm https://github.com/XJOC/marsa/releases/latest/download/install.ps1 | iex`.
  `marsa setup` runs once, with one UAC prompt: it installs a small helper
  service that keeps the sites' names in the hosts file, and trusts Marsa's
  certificate authority. PHP runs as one `php-cgi` per version. The shims run
  PHP, Node.js, and Composer as child programs: Ctrl+C reaches them and their
  exit code is kept. PHP trusts Windows' certificate roots and Marsa's
  authority. `marsa uninstall --all` removes it all, with one UAC prompt.
- **Doctor:** `marsa doctor` checks `.test` names on Linux and Windows, ports
  80 and 443 and the certificate trust on Windows, and the Microsoft Visual
  C++ runtime that PHP needs on Windows.
- **Releases** hold all five systems, and `install.ps1` next to `install.sh`,
  both listed in the signed `SHA256SUMS`.

### Changed

- Marsa's new window design is in progress. Its theme and its base widgets
  have landed: the window already uses the design's colors and its two fonts
  (Readex Pro and JetBrains Mono), in a light and a dark theme that follow
  your system setting. The screens get the new design in the next release.
  The fonts' licences are in `LICENSE`.
- Marsa's own programs start on a clean Windows without the Visual C++
  runtime. PHP for Windows still needs it; `marsa doctor` says so.

### Known limits

- Linux and Windows are a preview: no person has tried them yet.
- `marsa uninstall --all` does not run on Linux yet.
- The macOS programs are signed ad hoc, not notarized by Apple, and the Intel
  build has not been tried by a person yet. The Windows programs are not
  code-signed yet, so SmartScreen may warn.
- Without `minisign`, the install scripts check only the SHA-256 of the
  download, not its signature.

## [0.1.1] - 2026-11-18

A fix release for macOS.

### Fixed

- After you log out and in, a daemon left running from the earlier login is
  now replaced by a new one, so `marsa setup` and `marsa uninstall --all`
  can show the administrator prompt again. In 0.1.0 they stopped and asked
  you to run `marsa daemon stop` first.

### Changed

- The install command is now the script of the latest release:
  `curl -fsSL https://github.com/XJOC/marsa/releases/latest/download/install.sh | sh`.
  It installs the latest release, never a pre-release, unless
  `MARSA_VERSION` names another version.

### Known limits

- macOS only. Linux and Windows build in CI but are not released yet.
- The programs are signed ad hoc, not notarized by Apple.
- The Intel build has not been tried by a person yet.
- Without `minisign`, `install.sh` checks only the SHA-256 of the download,
  not its signature.

## [0.1.0] - 2026-11-14

The first release, for macOS (Apple Silicon and Intel).

### Added

- **Runtimes:** `marsa install`, `marsa runtimes`, `marsa use`, and
  `marsa uninstall` for PHP, Node.js, Composer, and Caddy. Marsa's PHP builds
  include the extensions Laravel needs, and Xdebug. Every download comes from
  a list signed with minisign, and each file is checked before use.
- **Shims:** `php`, `node`, `npm`, `npx`, and `composer` run the version each
  project asks for: a version set for the site, `.php-version`, `.nvmrc`,
  `.node-version`, `.tool-versions`, `mise.toml`, `composer.json`,
  `package.json`, or the default.
- **Sites:** `marsa park`, `forget`, `link`, `unlink`, `sites`, `open`,
  `secure`, `unsecure`, `isolate`, and `unisolate`. Every site is
  `https://<name>.test`, over HTTPS by default.
- **Setup:** `marsa setup` sends `.test` names to Marsa, routes ports 80 and
  443 to Caddy, and trusts Marsa's certificate authority for your user, with
  one administrator prompt and one macOS approval.
- **Daemon:** `marsad` runs Caddy and one PHP-FPM pool per PHP version.
  `marsa daemon`, `marsa services`, and `marsa logs` control and show them.
  `marsa autostart` starts Marsa at login.
- **Xdebug:** `marsa xdebug on` and `off`.
- **Doctor:** `marsa doctor` checks the daemon, the runtimes, Caddy, `.test`
  names, ports 80 and 443, the certificate trust, `PATH`, and the daemon's
  log, and says how to fix each problem.
- **Window and menu-bar icon:** `marsa-gui` shows the sites and the PHP
  versions, installs runtimes, and parks and links folders. `marsa-tray`
  shows Marsa's state and switches the default PHP.
- **Install and uninstall:** `install.sh`, and `marsa uninstall --all`, which
  asks before it removes anything and keeps the data folder unless you say
  otherwise.
- **Releases:** archives for macOS with `SHA256SUMS` and a minisign
  signature.

### Known limits

- macOS only. Linux and Windows build in CI but are not released yet.
- The programs are signed ad hoc, not notarized by Apple.
- The Intel build has not been tried by a person yet.
- Without `minisign`, `install.sh` checks only the SHA-256 of the download,
  not its signature.
- After you log out and in, a daemon left running from the earlier login
  cannot show the administrator prompt, so `marsa setup` and
  `marsa uninstall --all` say so and stop. Run `marsa daemon stop`, then the
  command again.

[0.2.0]: https://github.com/XJOC/marsa/releases/tag/v0.2.0
[0.1.1]: https://github.com/XJOC/marsa/releases/tag/v0.1.1
[0.1.0]: https://github.com/XJOC/marsa/releases/tag/v0.1.0
