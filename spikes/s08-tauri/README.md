# S-08 spike · minimal Tauri window

Throwaway spike for task S-08a: proves that a small Tauri 2 app (Arabic RTL
text, bundled IBM Plex fonts, one sidecar binary, ad-hoc signing on macOS)
builds in CI and runs on macOS, Windows, and Linux **before** any product
code is written. This code never enters the product. Results land in ADR 0005.

## What is inside

- `src-tauri/` — the Tauri app (`s08-app`), identifier `dev.marsa.spike-s08`.
  Two commands only: `diagnostics` and `run_sidecar`. No plugin permissions.
- `s08-sidecar/` — prints one JSON line and exits 0; embedded via
  `bundle.externalBin`.
- `src/` — React + TypeScript, Arabic-first RTL, CSS logical properties only.
- `fonts/` — IBM Plex Sans Arabic and IBM Plex Sans, complete `woff2` builds
  from the [`IBM/plex`](https://github.com/IBM/plex) releases
  `@ibm/plex-sans-arabic@1.1.0` and `@ibm/plex-sans@1.1.0`, license OFL-1.1
  (see each folder's `OFL.txt`).

## Local build

Prerequisites: the Rust toolchain in `rust-toolchain.toml`, Node (see
`.nvmrc`), and pnpm (the version pinned in `package.json`).

```sh
pnpm install
pnpm tauri dev     # dev window (vite on :5173)
pnpm tauri build   # release bundles for your OS, ad-hoc signed on macOS
```

Both commands run `scripts/prepare-sidecar.mjs` first: it builds
`s08-sidecar` and copies it to `src-tauri/binaries/s08-sidecar-<triple>`,
the name Tauri's `externalBin` expects.

CI: `.github/workflows/spike-s08.yml` builds the same app on
`macos-latest`, `windows-latest`, `ubuntu-22.04`, and `ubuntu-22.04-arm`.
