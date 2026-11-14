# Changelog

All notable changes to Marsa are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

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

[0.1.0]: https://github.com/XJOC/marsa/releases/tag/v0.1.0
