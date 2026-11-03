# S-09 spike · Tauri vs egui (macOS)

Throwaway spike for task S-09: the same three-tab screen built with Tauri 2
and with egui, measured the same way on a Mac (memory, first frame, scroll
smoothness, idle CPU, size). The results go into ADR 0007. This code never
enters the product.

## Layout

| Path | What it is |
| --- | --- |
| `Cargo.toml` | Standalone workspace and the one release profile for every program |
| `common/` | `s09-common`: deterministic data and the `--bench` helpers, shared by all programs |
| `tray/` | `tray`: menu-bar icon (`tray-icon` + `tao`) that starts the UI programs |
| `egui-app/` | `egui-app`: eframe window, `--renderer glow` or `--renderer wgpu` |
| `tauri-app/` | `tauri-app`: Tauri 2 + React 19 + TypeScript + Vite (`src-tauri/` is the Rust side); table and logs on a canvas grid, or the S-09 DOM lists with `--grid dom` |
| `measure.sh` | Builds everything, runs cases A to E, prints one table |
| `responsible-pids.js` | Helper for `measure.sh`: finds the WebKit processes of the Tauri app |

## Build

Prerequisites: the toolchain in `rust-toolchain.toml`, Node 22 (`.nvmrc`),
pnpm (version pinned in `tauri-app/package.json`).

```sh
cargo build --release -p tray
cargo build --release -p egui-app
(cd tauri-app && pnpm install && pnpm tauri build --bundles app)
```

Build one package per `cargo` call: Cargo then does not merge features
between programs, so each binary holds only what it needs. `measure.sh` does
the same. The release profile (`opt-level = 3`, `lto = true`,
`codegen-units = 1`, `strip = true`) is in the workspace `Cargo.toml`, so
`tauri build` uses it too.

## Run

```sh
target/release/tray                                    # menu-bar icon
target/release/egui-app --renderer glow                # or --renderer wgpu
target/release/bundle/macos/tauri-app.app/Contents/MacOS/tauri-app
target/release/bundle/macos/tauri-app.app/Contents/MacOS/tauri-app --with-tray
target/release/bundle/macos/tauri-app.app/Contents/MacOS/tauri-app --grid dom
```

Add `--bench` to any of them for the automatic walk (below).

- **tauri-app --grid canvas|dom** (S-09b): `canvas` is the default and draws the
  Table and Logs tabs on a canvas with `@glideapps/glide-data-grid`. `dom` keeps
  the S-09 lists (`@tanstack/react-virtual`, one DOM element per visible row),
  so both can be measured in one run. Any other value exits with an error. The
  tray starts the default (canvas).

- **tray** finds the UI programs next to its own executable (`target/release/`):
  Tauri at `bundle/macos/tauri-app.app/Contents/MacOS/tauri-app` when the `.app`
  exists, otherwise `tauri-app`; egui at `egui-app`. Each menu item starts a
  separate process with an absolute path; the process ends when its window
  closes, and the tray reaps it.
- **tauri-app --with-tray**: a tray icon lives in the same process. Closing the
  window destroys it (Tauri's default, not hide); the process stays alive with
  the tray (`RunEvent::ExitRequested` with no exit code is prevented). The tray
  menu can open the window again, or quit.

### Content (the same in both apps)

All values come from `s09-common`, as a pure function of the row index, so
both apps show identical data and neither stores the 100,000 rows:

- **Table**: 100,000 rows x 8 columns. egui: `egui_extras::TableBuilder`, only
  visible rows are built. Tauri canvas (default): `@glideapps/glide-data-grid`
  (row height 20, striped); the grid's visible-region callback asks
  `BatchCache` for the rows, which fetches them from Rust with `invoke` in
  batches of 500 (two batches prefetched on each side, the last 24 kept), and
  `updateCells` redraws the visible cells when a batch arrives. Tauri DOM
  (`--grid dom`): `@tanstack/react-virtual`, the same batches (one batch
  prefetched on each side, the last 12 kept).
- **SQL**: 5,000 lines. egui: `TextEdit` with `egui_extras` syntect
  highlighting. Tauri: CodeMirror 6 with `@codemirror/lang-sql`; the text is
  fetched from Rust when the tab opens.
- **Logs**: 50,000 lines. egui: `ScrollArea::show_rows`. Tauri canvas: one
  wide glide-data-grid column, row height 18, no header. Tauri DOM:
  `@tanstack/react-virtual`. Both fetch batches of 500 from Rust.

## `--bench`

The program walks the phases by itself and prints one line per phase on
stdout, flushed at once: `PHASE <name> key=value ...`. After each line it
pauses 5 s (`PHASE_PAUSE` in `common/src/bench.rs`), so `measure.sh` samples
memory at the right moment and the previous phase settles.

| Phase | Meaning | Keys |
| --- | --- | --- |
| `first-frame` | First frame with table rows painted | `ms` since the first line of `main()` |
| `table-scrolled` | Table scrolled top to bottom in 5 s | `frames p50 p95 max` (ms); egui adds `cpu_p50 cpu_p95 cpu_max`; Tauri adds `placeholder_frames` |
| `sql-loaded` | SQL tab opened, 5,000 lines loaded and painted | `ms` from opening the tab, `lines` |
| `sql-typing` | 50 characters typed, one per frame, at the start of line 2,500 (`TYPING_TEXT`, `TYPING_LINE` in `s09-common`) | `frames p50 p95 max` (ms) |
| `logs-scrolled` | Logs scrolled top to bottom in 5 s | same as the table |
| `idle-start` | Start of 10 s without activity | none |
| `idle` | End of the idle phase | `secs`; egui adds `repaints` (frames during idle, should be 0) |
| `window-closed` | Only `tauri-app --with-tray`: 5 s after the window was destroyed | `settle_secs` |
| `done` | The program exits | none |

`tray --bench` prints `ready` (icon created, `ms` since `main()`), `idle-start`,
`idle`, `done`.

In `--bench` mode the window floats above all others. Both eframe and WebKit
stop drawing a window that another window covers, and a script cannot always
bring an app to the front on macOS.

### How each number is measured

- **First frame.** egui: at the start of the second frame (`cumulative_frame_nr`
  is 1, so the first frame has been painted and presented). Tauri: React
  renders the first rows from Rust, then two `requestAnimationFrame` callbacks
  later (after the paint) the UI calls `bench_phase`, and Rust prints the time
  since `main()` from an `Instant` taken on its first line.
- **Frame interval** = wall time between two consecutive frames while the
  scroll runs, then p50, p95, max (nearest rank, `s09-common`, the same code
  for both). egui: the time between the starts of consecutive frames, with a
  repaint requested on every frame. Tauri: the difference between consecutive
  `requestAnimationFrame` timestamps, sent to Rust at the end. The first frame
  shows the top (warm-up), timing starts on the second. Each frame sets the
  scroll offset to `progress x max`. In Tauri the code also sends a `scroll`
  event at once, so the virtualizer renders the new rows in the same frame,
  like egui. Without it the rows would lag one frame behind.
- **Canvas grid scroll** (Tauri, default): glide-data-grid draws one canvas
  under a transparent native scroll element (`.dvn-scroller`) whose spacers
  are hidden. The bench scrolls that element exactly like the DOM list (same
  offsets, same immediate `scroll` event), and the grid redraws the canvas for
  the new offset.
- **SQL typing** (Tauri): the cursor goes to the start of line `TYPING_LINE`
  and is scrolled into view, then each frame inserts one character of
  `TYPING_TEXT` through CodeMirror (`view.dispatch` with the change, the new
  cursor, and `scrollIntoView`), like a key press. The first frame is a
  warm-up; the result has one interval per character. The editor is then
  blurred, so its cursor cannot wake the idle phase. Same text and line in
  egui.
- **egui CPU per frame**: `frame.info().cpu_usage` (eframe's CPU time for the
  previous frame, UI and rendering, without the vsync wait).
- **placeholder_frames** (Tauri): frames where a visible row was still waiting
  for its batch from Rust. DOM: rows inside the viewport; canvas: the rows of
  the grid's visible region, as the grid reported it for that frame.
- **Memory** (`measure.sh`): `footprint -f bytes --noCategories <pids>`, the
  `phys_footprint` of each process (the kernel's number, the same one Activity
  Monitor shows as "Memory"), in MiB. For Tauri: the app process plus its
  WebKit processes (WebContent, GPU, Networking).
- **Finding the WebKit processes.** They are XPC services: their parent is
  `launchd`, not the app. macOS records a "responsible process" for each
  process, and Activity Monitor groups processes with it.
  `responsible-pids.js` (JXA, run by `osascript`, no sudo) calls the libSystem
  function `responsibility_get_pid_responsible_for_pid`. A
  `com.apple.WebKit.*` process belongs to the app when its responsible PID is
  the app, or the app's own responsible process. When the app is started from
  a terminal, the terminal app is responsible for the app *and* for its WebKit
  processes. To exclude WebKit processes of other apps, `measure.sh` also drops
  every WebKit process that existed before the app started. If the lookup
  fails, the script falls back to "started after the app" and says so.
- **Idle CPU**: cumulative CPU time (`ps -o time=`) of the same processes at
  `idle-start` and at `idle`, divided by the wall time between the two samples.
  The result is a percent of one core.
- **Size**: the stripped release binary; for Tauri also the `.app` (`du -sk`).

## `measure.sh`

```sh
./measure.sh                       # build, then cases A-F, 1 warm-up + 3 runs each, medians
./measure.sh --cases BCDF          # S-09b: egui glow, egui wgpu, Tauri canvas, Tauri DOM
./measure.sh --quick --cases B     # build, then one run of case B
./measure.sh --no-build --runs 3   # reuse the built programs
./measure.sh --help
```

| Case | Command |
| --- | --- |
| A | `tray --bench` (icon only, no window) |
| B | `egui-app --renderer glow --bench` |
| C | `egui-app --renderer wgpu --bench` |
| D | `tauri-app --bench` (from the `.app`): canvas grid, the default since S-09b |
| E | `tauri-app --with-tray --bench`; the table adds memory after the window is closed |
| F | `tauri-app --grid dom --bench`: the S-09 DOM lists. Shown as `D-dom` in the table |

The table has one row per case and these columns: size, first frame, memory
at each phase (start, table, SQL, logs, idle), `Peak scroll MiB`, memory after
close (E), table frame p50/p95/max, egui CPU per frame, logs frame p95, SQL
load time, `SQL typing p50 / p95 / max ms`, and idle CPU. `Peak scroll MiB` is
the larger of the two samples taken when the table scroll and the logs scroll
end, computed per run, then the median. It is not polled during the scroll,
so the true peak can be higher.

The script needs no sudo: `footprint` and `ps` read processes of the same user,
and the WebKit processes run as that user. It writes `results/<timestamp>/`:
`samples.tsv` (every sample of every run), `medians.tsv`, `table.md`, and the
stderr of each run in `logs/`. A full run takes about 15 minutes. Do not use
the Mac or build anything while it runs.

The script is written for the bash 3.2 that ships with macOS.

## Known caveats

- **glow is not vsync-limited on macOS**: with continuous repaint it draws
  about 80 to 120 frames per second on a 60 Hz panel, so its intervals are
  often below 16.7 ms and it uses more CPU while scrolling. wgpu (Metal) holds
  60 Hz.
- **WebKit's `requestAnimationFrame` timestamps have 1 ms resolution**, so
  Tauri intervals read 16, 17, or 18 ms where egui reads 16.7.
- **A p95 of 16.7 ms or less is very hard to reach with vsync on.** A smooth
  60 Hz loop has intervals around 16.7 ms with about 1 ms of jitter, so its p95
  is usually 17 to 18 ms even with no dropped frame. A dropped frame shows as
  about 33 ms. The owner may want to read the decision rule with this in mind.
- **WebKit tile surfaces while scrolling (DOM grid, case D-dom)**: the synthetic scroll moves about
  6,600 px per frame, so every tile is new on every frame. WebContent then
  holds more than 1 GiB of 512x512 tile surfaces (much of it volatile and
  reclaimable), and releases it about 3 s after the scroll stops. The same
  happens at a 6 times slower scroll. The D-dom `Mem table` and `Mem logs`
  columns show this peak. The 5 s pause keeps it out of `sql-loaded` and
  `idle`.

- **Canvas grid (S-09b): the tile surfaces are gone, but WebKit memory still
  grows.** With glide-data-grid, WebContent graphics memory stays at about 40
  to 85 MB while scrolling (S-09 DOM: about 1.65 GiB). But WebContent's
  "WebKit malloc" grows to about 1.0 to 1.5 GiB during the table sweep and
  stays at about 1 GiB at idle (about 0.4 to 0.5 GiB of it reclaimable).
  Experiments, each one run: with every cell showing the same text, it stays
  at 75 to 100 MB. glide's `"direct"` render strategy (no canvas copies), a
  CPU canvas (`willReadFrequently`), or no row striping change nothing. So
  the growth comes from drawing many distinct strings with canvas `fillText`
  inside WebKit (about 13 KB per call during the sweep), not from glide's
  canvas copies or from the data. The canvas table also shows one stall of
  about 420 to 450 ms near 65% of the sweep (two runs), which fits a garbage
  collection of that heap.

## Manual check (owner, 5 minutes)

Start `target/release/tray`, open each program from the menu-bar icon, then
scroll the table fast, type in the middle of the SQL editor, and scroll the
logs. For each program, note "smooth" or "stutters", and any remark.

S-09b: in egui glow, type in the middle of the SQL editor: is it smooth now?
In Tauri (canvas, the tray's default), scroll the table fast: is the flicker
gone?
