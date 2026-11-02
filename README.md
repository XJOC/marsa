# Marsa

Marsa (Arabic: مرسى, "harbor") is an open-source, cross-platform desktop app for
local PHP and Node development: runtime version switching, local `.test` sites
with HTTPS, and, in later phases, databases, mail, queues, logs, and sharing.

**Status:** Phase 1 (core and daemon). Nothing runs end to end yet. The agreed
design is in [`docs/architecture.md`](docs/architecture.md).

## Layout

| Path | Package | What it is |
| --- | --- | --- |
| `crates/core/` | `marsa-core` | All logic, as modules. A functional core: data in, data or a plan out. |
| `crates/app/` | `marsa` | Two binaries: `marsa` (CLI) and `marsad` (daemon). |
| `crates/shim/` | `marsa-shim` | Runs the real `php`, `node`, ... at the version a project asks for. |
| `crates/helper/` | `marsa-helper` | Privileged helper with a closed list of admin operations. |
| `xtask/` | `xtask` | Repository chores, run as `cargo xtask <command>`. |
| `spikes/` | — | Throwaway experiments; each is its own workspace. |

## Build

Rust 1.99.1 is pinned in `rust-toolchain.toml`; `rustup` installs it on the
first command. `cargo-nextest` and `cargo-deny` are needed for the checks.

```sh
cargo build --workspace
cargo run --bin marsa -- --version   # prints: marsa 0.0.0
```

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
cargo deny check
cargo build -p marsa-shim --release
cargo tree -p marsa-shim -e normal   # must not contain tokio
```

`marsa-core` has a `full` feature, on by default, that gates the three modules
which touch processes or the network. `marsa-shim` uses `default-features =
false`, so a release shim stays small.

## License

Apache-2.0. See [`LICENSE`](LICENSE).

---

## بالعربية

**مرسى** تطبيق مكتبي مفتوح المصدر لإعداد بيئة تطوير PHP وNode محلياً: تبديل
إصدارات الأدوات، ومواقع `.test` محلية بشهادة HTTPS، ثم قواعد البيانات والبريد
والطوابير والسجلات في مراحل لاحقة.

المشروع في المرحلة 1 (النواة والخدمة الخلفية)، ولا شيء يعمل من أوله إلى آخره
بعد. التصميم في `docs/architecture.md`، وقواعد العمل في `AGENTS.md`.

للتحقق من البناء: `cargo build --workspace` ثم `cargo run --bin marsa -- --version`.
الرخصة: Apache-2.0.
