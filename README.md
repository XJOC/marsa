# Marsa

![Phase 1: 98%](https://img.shields.io/badge/Phase%201-98%25-brightgreen) ![Phase 2: 20%](https://img.shields.io/badge/Phase%202-20%25-lightgrey)

Marsa (Arabic: مرسى, "harbor") runs your PHP and Node.js projects on your
computer as `https://<folder>.test` sites. It installs the PHP, Node.js, Composer, and Caddy
versions you need, picks the right version for each project, and gives every
site a trusted HTTPS certificate. It also installs and runs MySQL, PostgreSQL,
and Valkey (a Redis-compatible server) for your projects, with no Docker. It is
open source (Apache-2.0) and works from the terminal or from its window.

**Status: 0.3.0.** macOS is stable. It is tested on Apple Silicon; the Intel
build passes the same automated checks. Linux and Windows are a preview: tested
end to end on GitHub runners, not yet by a person.

## Install

### macOS and Linux

```sh
curl -fsSL https://github.com/XJOC/marsa/releases/latest/download/install.sh | sh
```

The script installs Marsa for your user only, in
`~/Library/Application Support/dev.marsa.Marsa` on macOS, and in
`${XDG_DATA_HOME:-~/.local/share}/marsa` on Linux. It never uses `sudo` and
never edits your shell files. Before it unpacks anything, it checks the
SHA-256 of the download, and the [signature](#verify-a-download) too if
`minisign` is installed. It installs the latest release, never a
pre-release; to install another version, run it with `MARSA_VERSION=0.1.0 sh`.
To read the script first, download it, read it, then run it:

```sh
curl -fsSLO https://github.com/XJOC/marsa/releases/latest/download/install.sh
sh install.sh
```

Then add Marsa to your `PATH`, as the script prints: add this line at the end
of `~/.zshrc` (with bash, the file the script names, usually
`~/.bash_profile` on macOS and `~/.bashrc` on Linux), and open a new terminal.
On macOS the line is:

```sh
export PATH="$HOME/Library/Application Support/dev.marsa.Marsa/shims:$HOME/Library/Application Support/dev.marsa.Marsa/bin:$PATH"
```

On Linux it is:

```sh
export PATH="$HOME/.local/share/marsa/shims:$HOME/.local/share/marsa/bin:$PATH"
```

(or under `$XDG_DATA_HOME/marsa` if you set `XDG_DATA_HOME`; the script prints
the line for your system).

On Linux, the window needs a desktop session with Vulkan or OpenGL. The
menu-bar icon needs GTK 3 and AppIndicator: on Debian and Ubuntu, run
`sudo apt install libgtk-3-0 libayatana-appindicator3-1`.

`shims/` holds `php`, `node`, `npm`, `npx`, and `composer`. Each one runs the
version the current project asks for. `bin/` holds `marsa` and the other
Marsa programs.

### Windows (preview)

In PowerShell:

```powershell
irm https://github.com/XJOC/marsa/releases/latest/download/install.ps1 | iex
```

The script installs Marsa for your user only, in `%LOCALAPPDATA%\marsa\Marsa\data`.
It needs no admin rights. It checks the SHA-256 of the download before it
unpacks anything, and sets your user `PATH` itself: open a new terminal
afterwards. To read the script first, download it and read it:

```powershell
irm https://github.com/XJOC/marsa/releases/latest/download/install.ps1 -OutFile install.ps1
```

When you have read it, run it with `Get-Content .\install.ps1 -Raw | iex`.

## Quick start

```sh
marsa install php 8.4      # the first PHP you install becomes the default
marsa install caddy        # the web server behind the .test sites
marsa park ~/Code          # every folder in ~/Code becomes a site
marsa setup                # HTTPS and .test names, once (see below)
marsa open app             # opens https://app.test, the folder ~/Code/app
```

For a database, install the server. It starts at once, and starts again each
time Marsa starts:

```sh
marsa install mysql        # or postgres, or valkey
marsa services             # shows each server and how to connect to it
```

`marsa setup` runs once. The system asks for your administrator password
once (on Windows, the admin prompt); macOS then also asks you to approve
trusting Marsa's certificate authority. Run it in a terminal on your desktop,
not over SSH, so that these dialogs can show.

If something does not work, run `marsa doctor`. It checks each part, says what
is wrong, and says how to fix it. It changes nothing.

### The window and the menu-bar icon

`marsa-gui` opens Marsa's window: Home, Sites, Runtimes (PHP, Node.js,
Composer, Caddy, and the database servers), Services, Logs, and Settings.
`marsa autostart on` starts the menu-bar icon (`marsa-tray`) when you log in.
Click it for a small panel with your sites, your PHP version, Xdebug, and Stop
and Quit (on Linux, a menu). Both programs are in `bin/`.

## Databases

`marsa install mysql`, `marsa install postgres`, and `marsa install valkey` work
like `marsa install php`:

| Server | Versions | Default port | Source |
| --- | --- | --- | --- |
| MySQL | 8.4 | 3306 | Oracle's builds, checked against Oracle's signature |
| PostgreSQL | 17 and 18 | 5432 | the [theseus-rs](https://github.com/theseus-rs/postgresql-binaries) builds, checked against their GitHub attestation |
| Valkey | 8.1 | 6379 | built by Marsa from the Valkey source |

The first server you install starts at once and starts again with Marsa. Each
listens on `127.0.0.1` only. MySQL and PostgreSQL have a `root` user with no
password; Valkey has no password. `marsa services` prints the lines to copy
into a Laravel project's settings file:

```
mysql: 127.0.0.1:3306, user root, no password. For a Laravel .env:
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_PORT=3306
DB_USERNAME=root
DB_PASSWORD=
```

Valkey's lines are `REDIS_HOST=127.0.0.1` and `REDIS_PORT=6379`, for
Laravel's cache and queue. Marsa's PHP has the `redis`, `pdo_mysql`, and
`pdo_pgsql` extensions.

After you install a server, its command-line programs are in your `PATH` and
already point to Marsa's server: `mysql`, `mysqldump`, `mysqladmin`, `psql`,
`pg_dump`, `pg_restore`, `valkey-cli`, and `redis-cli`. Marsa creates them only
after you install a server, so it never hides a `psql` that you installed
yourself before.

`marsa services stop mysql` stops a server, and `marsa services start mysql`
starts it. `marsa uninstall mysql 8.4.13` removes the program and keeps your
databases (stop the server first). On an Intel Mac, MySQL is 8.4.11: Oracle has
no newer Intel build. On Linux, MySQL needs `/usr/bin/xz`, and `marsa doctor`
says if it is missing. On Windows, PostgreSQL refuses to start from an
administrator account: use your normal user.

## Which version a project gets

The first match wins:

1. The version you set for a site: `marsa isolate app 8.3`.
2. A version file in the project folder or a folder above it: `.php-version`,
   `.nvmrc`, `.node-version`, `.tool-versions`, or `mise.toml`. Files that nvm,
   asdf, or mise use keep working.
3. `require.php` in `composer.json`, or `engines.node` in `package.json`: the
   highest installed version that fits.
4. The default: `marsa use php 8.4`, `marsa use node 22`.

In a terminal, the same rules choose which `php` or `node` runs. A version is
used only once it is installed: `marsa install php 8.3`.

## Commands

| Command | What it does |
| --- | --- |
| `marsa install <tool> [version]` | Install `php`, `node`, `composer`, `caddy`, `mysql`, `postgres`, or `valkey`: `8`, `8.4`, or `8.4.27`. Without a version: the newest. |
| `marsa runtimes [tool]` | List the installed versions and the defaults. |
| `marsa use <php\|node> <version>` | Make an installed version the default. |
| `marsa uninstall <tool> <version>` | Delete an installed version. Marsa refuses one that is in use. |
| `marsa park [folder]` | Serve every sub-folder as a site. Default: the current folder. |
| `marsa forget [folder]` | Stop serving a parked folder. |
| `marsa link [name] [--path folder]` | Serve one folder as a site. |
| `marsa unlink <name>` | Stop serving a linked site. |
| `marsa sites` | List the sites, and the folders that did not become one. |
| `marsa open <name>` | Open a site in the browser. |
| `marsa secure <name>`, `marsa unsecure <name>` | Serve a site over HTTPS (the default), or over plain HTTP. |
| `marsa isolate <name> <version>`, `marsa unisolate <name>` | Give a site its own PHP version, or remove it. |
| `marsa xdebug [on\|off]` | Turn Xdebug on or off in every installed PHP. |
| `marsa queue <on\|off> <name>`, `marsa queue status [name]` | Run `php artisan queue:work` for a Laravel site; Marsa restarts it if it crashes. `marsa schedule` does the same for `schedule:work`. |
| `marsa services [start\|stop\|restart <id>]` | List the services (Caddy, PHP, and your database servers), or control one. |
| `marsa logs <id> [-f]` | Show the output of a service, such as `caddy`. |
| `marsa daemon <start\|stop\|status>` | Control `marsad`, the program that runs in the background. |
| `marsa autostart [on\|off]` | Start Marsa at login, or not. |
| `marsa setup` | HTTPS and `.test` names: once, with your password. |
| `marsa doctor` | Check everything and say how to fix what is wrong. |
| `marsa uninstall --all [--delete-data]` | Remove Marsa from this Mac (see below). |

`marsa <command> --help` explains each one.

## Herd, Valet, and other local servers

Marsa needs ports 80 and 443, and the `.test` names.

- **Ports.** If another program, such as Herd's or Valet's nginx, holds port
  80 or 443, `marsa setup` does not route those ports to Marsa, and it says
  which program holds them. Stop that program's web server (in Herd, stop its
  services; with Valet, run `valet stop`), then run `marsa setup` again.
- **`.test` names.** If another tool already owns `/etc/resolver/test`, Marsa
  leaves that file alone. That tool then answers the `.test` names, which
  works as long as it sends them to `127.0.0.1`.

Run one of these tools at a time. `marsa doctor` names the program that is in
the way.

## Uninstall

```sh
marsa uninstall --all
```

It asks before it removes anything. Then it stops Marsa and removes what
`marsa setup` added to the system (macOS asks for your password once). It
also asks whether to delete Marsa's data folder: the installed runtimes, the
settings, and the certificate authority. `--delete-data` deletes the folder
without that question, and `--yes` answers the first one. At the end, take
Marsa out of your `PATH`: on macOS and Linux, delete the line from `~/.zshrc`
(and from `~/.zprofile` if you added it there for 0.1.0-rc.1); on Windows,
remove Marsa's two folders from your user's environment variables (search for
"environment variables" in Settings).

This works on macOS, Windows, and Linux (Linux and Windows are a preview). On
Linux it also removes the certificate authority that `marsa setup` added to
Chrome and Firefox for this data folder.

## Verify a download

Each release has `SHA256SUMS`, which lists the SHA-256 of every file, and
`SHA256SUMS.minisig`, its [minisign](https://jedisct1.github.io/minisign/)
signature. To check them yourself, in the folder with the downloads:

```sh
minisign -V -P RWTuvZZrimY2ASzeaoWovb0LKYQTrVnNIbXoxoTKGJEZmSeLlyomDKxU -m SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
```

The trusted comment that `minisign` prints must be `marsa release v<version>`.
The same key signs the list of PHP, Node.js, Composer, and Caddy versions that
Marsa installs.

## Security

See [`SECURITY.md`](SECURITY.md): how to report a problem, and what runs with
administrator rights.

## Contributing

Marsa is a Rust workspace. Rust 1.99.1 is pinned in `rust-toolchain.toml`;
`rustup` installs it on the first command. The checks also need
`cargo-nextest` and `cargo-deny`.

| Path | Package | What it is |
| --- | --- | --- |
| `crates/core/` | `marsa-core` | All logic, as modules. A functional core: data in, data or a plan out. |
| `crates/app/` | `marsa` | Two programs: `marsa` (the CLI) and `marsad` (the daemon). |
| `crates/shim/` | `marsa-shim` | Runs the real `php`, `node`, ... at the version a project asks for. |
| `crates/helper/` | `marsa-helper` | The administrator helper, with a closed list of operations. |
| `crates/gui/` | `marsa-gui` | The window (`marsa-gui`) and the menu-bar icon (`marsa-tray`). |
| `xtask/` | `xtask` | Repository chores, run as `cargo xtask <command>`. |
| `spikes/` | — | Throwaway experiments; each is its own workspace. |

```sh
cargo build --workspace
cargo run --bin marsa -- --version   # prints: marsa 0.3.0
```

The checks that every pull request passes:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
cargo deny check
cargo build -p marsa-shim --release
cargo tree -p marsa-shim -e normal   # must not contain tokio
```

`marsa-core` has a `full` feature, on by default, that gates the modules that
start processes or use the network. `marsa-shim` uses `default-features =
false`, so the shim stays small and fast.

## License

Apache-2.0. See [`LICENSE`](LICENSE).

---

## بالعربية

**مرسى** يشغّل مشاريع PHP وNode.js على جهاز Mac كمواقع `https://<اسم-المجلد>.test`.
يثبّت إصدارات PHP وNode.js وComposer وCaddy، ويختار لكل مشروع إصداره،
ويعطي كل موقع شهادة HTTPS موثوقة. ويثبّت MySQL وPostgreSQL وValkey ويشغّلها
بلا Docker (`marsa install mysql`). يعمل من الطرفية أو من نافذته.

الإصدار 0.3.0: macOS مستقر. Linux وWindows نسخة تجريبية (preview)،
اختُبرتا على خوادم GitHub ولم يجرّبهما شخص بعد.

التثبيت على macOS وLinux: `curl -fsSL https://github.com/XJOC/marsa/releases/latest/download/install.sh | sh`
وعلى Windows (PowerShell): `irm https://github.com/XJOC/marsa/releases/latest/download/install.ps1 | iex`،
ثم سطر `PATH` الذي يطبعه السكربت، ثم `marsa install php 8.4` و`marsa setup`.
إن لم يعمل شيء: `marsa doctor`. الرخصة: Apache-2.0.
