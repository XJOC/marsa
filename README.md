# Marsa

Marsa (Arabic: مرسى, "harbor") runs your PHP and Node.js projects on your Mac as
`https://<folder>.test` sites. It installs the PHP, Node.js, Composer, and Caddy
versions you need, picks the right version for each project, and gives every
site a trusted HTTPS certificate. It is open source (Apache-2.0) and works
from the terminal or from its window.

**Status: 0.1.0, a first release for macOS.** It is tested on Apple Silicon.
The Intel build passes the same automated checks, but no person has tried it
yet. Linux and Windows are coming; they build in CI but are not released yet.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/XJOC/marsa/main/install.sh | sh
```

The script installs Marsa for your user only, in
`~/Library/Application Support/dev.marsa.Marsa`. It never uses `sudo` and
never edits your shell files. Before it unpacks anything, it checks the
SHA-256 of the download, and the [signature](#verify-a-download) too if
`minisign` is installed. You can read the script first:
[`install.sh`](install.sh).

Then add Marsa to your `PATH`, as the script prints: add this line at the end
of `~/.zshrc` (with bash, the file the script names, usually
`~/.bash_profile`), and open a new terminal.

```sh
export PATH="$HOME/Library/Application Support/dev.marsa.Marsa/shims:$HOME/Library/Application Support/dev.marsa.Marsa/bin:$PATH"
```

`shims/` holds `php`, `node`, `npm`, `npx`, and `composer`. Each one runs the
version the current project asks for. `bin/` holds `marsa` and the other
Marsa programs.

## Quick start

```sh
marsa install php 8.4      # the first PHP you install becomes the default
marsa install caddy        # the web server behind the .test sites
marsa park ~/Code          # every folder in ~/Code becomes a site
marsa setup                # HTTPS and .test names, once (see below)
marsa open app             # opens https://app.test, the folder ~/Code/app
```

`marsa setup` runs once. macOS asks for your administrator password, then asks
you to approve trusting Marsa's certificate authority. Run it from Terminal
on the Mac's desktop, not over SSH, so that macOS can show these dialogs.

If something does not work, run `marsa doctor`. It checks each part, says what
is wrong, and says how to fix it. It changes nothing.

### The window and the menu-bar icon

`marsa-gui` opens Marsa's window: sites, PHP versions, installs, and settings.
`marsa autostart on` starts the menu-bar icon (`marsa-tray`) when you log in.
Both programs are in `bin/`.

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
| `marsa install <tool> [version]` | Install `php`, `node`, `composer`, or `caddy`: `8`, `8.4`, or `8.4.27`. Without a version: the newest. |
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
| `marsa services [start\|stop\|restart <id>]` | List the services (Caddy and PHP), or control one. |
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
without that question, and `--yes` answers the first one. At the end, remove
the `PATH` line from your shell's startup file: `~/.zshrc`, and `~/.zprofile`
if you added it there for 0.1.0-rc.1.

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
cargo run --bin marsa -- --version   # prints: marsa 0.1.0
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
ويعطي كل موقع شهادة HTTPS موثوقة. يعمل من الطرفية أو من نافذته.

الإصدار 0.1.0 لنظام macOS فقط. Linux وWindows قريباً.

التثبيت: `curl -fsSL https://raw.githubusercontent.com/XJOC/marsa/main/install.sh | sh`،
ثم سطر `PATH` الذي يطبعه السكربت، ثم `marsa install php 8.4` و`marsa setup`.
إن لم يعمل شيء: `marsa doctor`. الرخصة: Apache-2.0.
