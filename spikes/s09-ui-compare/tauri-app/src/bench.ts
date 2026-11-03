// The `--bench` walk for Tauri. It mirrors `egui-app/src/bench_run.rs`:
// same phases, same pauses (from Rust), same frame definition.
//
// Frame interval = the difference between the timestamps of consecutive
// `requestAnimationFrame` callbacks: WebKit calls them once per rendering
// update, so a slow update (React, layout, paint) makes the gap longer.
// Rust computes p50/p95/max with the same code as egui (`s09-common`).
import { api, type AppConfig } from "./api";

export type Tab = "table" | "sql" | "logs";
export type SignalName = "table-ready" | "sql-ready" | "logs-ready";

const fired = new Set<SignalName>();
const waiting = new Map<SignalName, Array<() => void>>();

/** Marks a one-time UI milestone (first rows painted, editor ready, ...). */
export function signal(name: SignalName): void {
  if (fired.has(name)) return;
  fired.add(name);
  for (const resolve of waiting.get(name) ?? []) resolve();
  waiting.delete(name);
}

function waitFor(name: SignalName): Promise<void> {
  if (fired.has(name)) return Promise.resolve();
  return new Promise((resolve) => {
    waiting.set(name, [...(waiting.get(name) ?? []), resolve]);
  });
}

/** A virtual list the bench can scroll. */
export interface Scroller {
  element: HTMLElement;
  /** True when a row in the viewport still waits for its batch. */
  hasMissingRows: () => boolean;
}

const scrollers = new Map<"table" | "logs", Scroller>();

export function registerScroller(
  name: "table" | "logs",
  scroller: Scroller,
): () => void {
  scrollers.set(name, scroller);
  return () => {
    if (scrollers.get(name) === scroller) scrollers.delete(name);
  };
}

/**
 * Runs `callback` once the current DOM has been painted: the first frame
 * callback runs before the paint, the second one after it.
 */
export function afterNextPaint(callback: () => void): void {
  requestAnimationFrame(() => requestAnimationFrame(() => callback()));
}

const sleep = (ms: number) =>
  new Promise<void>((resolve) => setTimeout(resolve, ms));

interface ScrollResult {
  intervals: number[];
  placeholderFrames: number;
}

/**
 * Scrolls from top to bottom in `durationMs`. The first frame shows the top
 * (warm-up); timing starts on the second, like egui. After each new offset a
 * `scroll` event is sent at once, so the virtualizer renders the new rows in
 * the same frame (egui also draws the rows of the new offset in the same
 * frame). Without it the rows would lag one frame behind.
 */
function scrollThrough(scroller: Scroller, durationMs: number): Promise<ScrollResult> {
  const { element } = scroller;
  const setOffset = (offset: number) => {
    element.scrollTop = offset;
    element.dispatchEvent(new Event("scroll"));
  };
  return new Promise((resolve) => {
    const intervals: number[] = [];
    let warmedUp = false;
    let start: number | null = null;
    let last = 0;
    let finishing = false;
    let placeholderFrames = 0;
    const tick = (now: number) => {
      if (start === null) {
        if (warmedUp) {
          start = now;
          last = now;
        } else {
          warmedUp = true;
        }
        setOffset(0);
        requestAnimationFrame(tick);
        return;
      }
      intervals.push(now - last);
      last = now;
      if (finishing) {
        resolve({ intervals, placeholderFrames });
        return;
      }
      const progress = Math.min(1, (now - start) / durationMs);
      setOffset(progress * (element.scrollHeight - element.clientHeight));
      if (scroller.hasMissingRows()) placeholderFrames++;
      finishing = progress >= 1;
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

function scroller(name: "table" | "logs"): Scroller {
  const found = scrollers.get(name);
  if (found === undefined) throw new Error(`no ${name} scroller`);
  return found;
}

export async function runBench(
  config: AppConfig,
  open: (tab: Tab) => void,
): Promise<void> {
  // 1. First frame with table rows painted. Rust measures from `main()`.
  await waitFor("table-ready");
  await api.benchPhase("first-frame");
  await sleep(config.pause_ms);

  // 2. Table, top to bottom.
  const table = await scrollThrough(scroller("table"), config.scroll_ms);
  await api.benchPhase("table-scrolled", table);
  await sleep(config.pause_ms);

  // 3. SQL tab: fetch the 5,000 lines from Rust, build the editor, paint.
  const sqlOpened = performance.now();
  open("sql");
  await waitFor("sql-ready");
  await api.benchPhase("sql-loaded", { ms: performance.now() - sqlOpened });
  await sleep(config.pause_ms);

  // 4. Logs, top to bottom.
  open("logs");
  await waitFor("logs-ready");
  const logs = await scrollThrough(scroller("logs"), config.scroll_ms);
  await api.benchPhase("logs-scrolled", logs);
  await sleep(config.pause_ms);

  // 5. Idle: Rust prints `idle-start`, waits, prints `idle`.
  await api.benchIdle();
  await sleep(config.pause_ms);

  // 6. Rust prints `done` and exits (with `--with-tray`: closes the window
  // first and prints `window-closed`).
  await api.benchFinish();
}
