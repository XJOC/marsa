# Changelog

All notable changes to Marsa are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [0.2.0] - Unreleased

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
