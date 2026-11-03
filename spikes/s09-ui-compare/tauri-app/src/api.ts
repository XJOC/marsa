// Typed wrappers around the Rust commands in `src-tauri/src/lib.rs`. The UI
// never holds the full data: it asks Rust for the rows it shows.
import { invoke } from "@tauri-apps/api/core";

/** One table row: 8 cells, in `AppConfig.table_columns` order. */
export type Row = string[];

/** Mirrors `AppConfig` in `lib.rs` (snake_case field names). */
export interface AppConfig {
  bench: boolean;
  with_tray: boolean;
  /** `--grid`: canvas grid (default) or the S-09 DOM lists. */
  grid: "canvas" | "dom";
  pause_ms: number;
  scroll_ms: number;
  table_rows: number;
  table_columns: string[];
  log_lines: number;
  batch_size: number;
  /** `sql-typing`: text typed one character per frame at the start of line `typing_line` (1-based). */
  typing_text: string;
  typing_line: number;
}

export type PhaseName =
  | "first-frame"
  | "table-scrolled"
  | "sql-loaded"
  | "sql-typing"
  | "logs-scrolled";

export interface PhaseData {
  intervals?: number[];
  ms?: number;
  placeholderFrames?: number;
}

export const api = {
  appConfig: () => invoke<AppConfig>("app_config"),
  tableRows: (start: number, count: number) =>
    invoke<Row[]>("table_rows", { start, count }),
  logLines: (start: number, count: number) =>
    invoke<string[]>("log_lines", { start, count }),
  sqlText: () => invoke<string>("sql_text"),
  uiError: (message: string) => invoke<void>("ui_error", { message }),
  benchPhase: (name: PhaseName, data: PhaseData = {}) =>
    invoke<void>("bench_phase", { name, ...data }),
  benchIdle: () => invoke<void>("bench_idle"),
  benchFinish: () => invoke<void>("bench_finish"),
};

/**
 * Sends an error to Rust, which prints it on stderr: a release webview has
 * no visible console. Never throws.
 */
export function reportError(context: string, error: unknown): void {
  const message = `${context}: ${error instanceof Error ? error.message : String(error)}`;
  api.uiError(message).catch(() => undefined);
}
